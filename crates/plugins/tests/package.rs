#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Plugin packages: reading untrusted zips, signatures, key pinning and
//! rotation. Signatures come from `tether_plugins::testing`, which signs
//! the way minisign does with throwaway keys.

use std::io::{Cursor, Write};

use tether_plugins::package::{self, PackageError, Trust};
use tether_plugins::testing::{COMPONENT, Key, manifest, zip};
use zip::write::SimpleFileOptions;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n rest of the image";

fn plugin_zip(key: &Key, extra: &[(&str, &[u8])]) -> Vec<u8> {
    tether_plugins::testing::package("nmu.test", key, extra).0
}

/// Reads and verifies in one go, as installs do.
fn verify(
    bytes: &[u8],
    signature: &str,
    pinned: Option<&str>,
) -> Result<package::Verified, PackageError> {
    package::read(bytes)?.verify(signature, pinned)
}

#[test]
fn a_good_package_reads() {
    let key = Key::new(1);
    let bytes = plugin_zip(
        &key,
        &[
            ("migrations/", b""),
            ("migrations/0002_add_index.sql", b"CREATE INDEX i ON t (a);"),
            ("migrations/0001_create.sql", b"CREATE TABLE t (a int);"),
            ("ui/", b""),
            ("ui/icon.png", PNG),
        ],
    );
    let unverified = package::read(&bytes).unwrap();
    assert_eq!(unverified.id(), "nmu.test");
    let package = unverified.package();
    assert_eq!(package.manifest.plugin.id, "nmu.test");
    assert_eq!(package.component, COMPONENT);
    let versions: Vec<_> = package
        .migrations
        .iter()
        .map(|m| (m.version, m.name.as_str()))
        .collect();
    assert_eq!(versions, [(1, "create"), (2, "add_index")]);
    assert_eq!(package.assets[0].name, "icon.png");
    assert_eq!(package.assets[0].content_type, "image/png");
}

#[test]
fn unexpected_entries_are_refused() {
    let key = Key::new(1);
    for name in [
        "../plugin.toml",
        "/etc/passwd",
        "ui/../plugin.toml",
        "ui/sub/icon.png",
        "ui/icon.svg",
        "ui/icon.html",
        "ui/Icon.png",
        "migrations/1_create.sql",
        "migrations/0001_create.SQL",
        "migrations/0001-create.sql",
        "README.md",
        "plugin.wasm.bak",
        "Plugin.toml",
        "ui\\icon.png",
        "ui/icon\0.png",
        "ui/ïcon.png",
    ] {
        let bytes = plugin_zip(&key, &[(name, PNG)]);
        match package::read(&bytes) {
            Err(PackageError::Entry { problem, .. }) => {
                assert!(problem.contains("isn't a file"), "{name}: {problem}");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn missing_or_malformed_parts_are_refused() {
    let key = Key::new(1);
    let manifest = manifest("nmu.test", &key);

    let no_wasm = zip(&[("plugin.toml", manifest.as_bytes())]);
    assert_eq!(
        package::read(&no_wasm).unwrap_err(),
        PackageError::Missing("plugin.wasm")
    );
    let no_manifest = zip(&[("plugin.wasm", COMPONENT)]);
    assert_eq!(
        package::read(&no_manifest).unwrap_err(),
        PackageError::Missing("plugin.toml")
    );
    let bad_manifest = zip(&[
        ("plugin.toml", b"[plugin]\nid = 3"),
        ("plugin.wasm", COMPONENT),
    ]);
    assert!(matches!(
        package::read(&bad_manifest),
        Err(PackageError::Manifest(_))
    ));

    let gap = plugin_zip(&key, &[("migrations/0002_second.sql", b"SELECT 1;")]);
    assert!(
        package::read(&gap)
            .unwrap_err()
            .to_string()
            .contains("no gaps")
    );
    let repeat = plugin_zip(
        &key,
        &[
            ("migrations/0001_a.sql", b"SELECT 1;"),
            ("migrations/0001_b.sql", b"SELECT 1;"),
        ],
    );
    assert!(
        package::read(&repeat)
            .unwrap_err()
            .to_string()
            .contains("no gaps")
    );
    let binary_sql = plugin_zip(&key, &[("migrations/0001_a.sql", b"\xff\xfe")]);
    assert!(
        package::read(&binary_sql)
            .unwrap_err()
            .to_string()
            .contains("UTF-8")
    );

    let fake_png = plugin_zip(&key, &[("ui/icon.png", b"<svg onload=alert(1)>")]);
    assert!(
        package::read(&fake_png)
            .unwrap_err()
            .to_string()
            .contains("image/png")
    );

    let half_rotation = plugin_zip(&key, &[("rotation.txt", b"x")]);
    assert!(matches!(
        package::read(&half_rotation),
        Err(PackageError::Rotation(_))
    ));

    assert!(matches!(
        package::read(b"not a zip at all, just some bytes"),
        Err(PackageError::NotAZip(_))
    ));
}

#[test]
fn sizes_are_capped_whatever_the_archive_claims() {
    let key = Key::new(1);
    // Compresses to almost nothing; the capped read stops it, not the
    // size the archive declares.
    let bomb = vec![b' '; package::MAX_MIGRATION_BYTES + 1];
    let bytes = plugin_zip(&key, &[("migrations/0001_big.sql", &bomb)]);
    assert!(bytes.len() < 10_000);
    let err = package::read(&bytes).unwrap_err().to_string();
    assert!(err.contains("bigger than"), "{err}");

    // Lie about the size: patch the declared uncompressed size in the
    // central directory down to 10 bytes. The read still stops at the cap
    // (and the CRC check fails either way).
    let mut lying = bytes.clone();
    let name_at = rfind(&lying, b"migrations/0001_big.sql");
    let header = name_at - 46;
    assert_eq!(&lying[header..header + 4], b"PK\x01\x02");
    lying[header + 24..header + 28].copy_from_slice(&10u32.to_le_bytes());
    assert!(package::read(&lying).is_err());

    let too_big = vec![0u8; package::MAX_PACKAGE_BYTES + 1];
    assert_eq!(package::read(&too_big).unwrap_err(), PackageError::TooLarge);

    let many: Vec<(String, Vec<u8>)> = (0..=package::MAX_ENTRIES)
        .map(|i| {
            (
                format!("migrations/{:04}_m.sql", i + 1),
                b"SELECT 1;".to_vec(),
            )
        })
        .collect();
    let manifest = manifest("nmu.test", &key);
    let mut files: Vec<(&str, &[u8])> = vec![("plugin.toml", manifest.as_bytes())];
    files.extend(many.iter().map(|(n, d)| (n.as_str(), d.as_slice())));
    let err = package::read(&zip(&files)).unwrap_err().to_string();
    assert!(err.contains("entries"), "{err}");
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .rposition(|w| w == needle)
        .unwrap()
}

#[test]
fn tricky_archives_are_refused() {
    let key = Key::new(1);

    // Two entries named plugin.toml: the zip crate would keep only one.
    let manifest = manifest("nmu.test", &key);
    let mut dup = zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.tomX", b"[plugin]\nid = \"nmu.other\""),
        ("plugin.wasm", COMPONENT),
    ]);
    while let Some(at) = find(&dup, b"plugin.tomX") {
        dup[at + 10] = b'l';
    }
    let err = package::read(&dup).unwrap_err().to_string();
    assert!(err.contains("more than once"), "{err}");

    // A symbolic link.
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file("plugin.toml", options).unwrap();
    writer.write_all(manifest.as_bytes()).unwrap();
    writer
        .add_symlink("plugin.wasm", "/etc/passwd", options)
        .unwrap();
    let link = writer.finish().unwrap().into_inner();
    let err = package::read(&link).unwrap_err().to_string();
    assert!(err.contains("symbolic link"), "{err}");

    // The encrypted flag, set on every header.
    let mut encrypted = plugin_zip(&key, &[]);
    for signature in [&b"PK\x03\x04"[..], b"PK\x01\x02"] {
        let offset = if signature == b"PK\x03\x04" { 6 } else { 8 };
        let mut from = 0;
        while let Some(at) = find(&encrypted[from..], signature) {
            encrypted[from + at + offset] |= 1;
            from += at + 4;
        }
    }
    let err = package::read(&encrypted).unwrap_err().to_string();
    assert!(err.contains("encrypted"), "{err}");

    // An archive comment (another place an end record could hide).
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.set_comment("PK\x05\x06 hello").unwrap();
    writer.start_file("plugin.toml", options).unwrap();
    writer.write_all(manifest.as_bytes()).unwrap();
    writer.start_file("plugin.wasm", options).unwrap();
    writer.write_all(COMPONENT).unwrap();
    let commented = writer.finish().unwrap().into_inner();
    assert!(matches!(
        package::read(&commented),
        Err(PackageError::NotAZip(_))
    ));

    // Something other than deflate.
    let mut stored_then_patched = plugin_zip(&key, &[]);
    let name_at = rfind(&stored_then_patched, b"plugin.wasm");
    let header = name_at - 46;
    stored_then_patched[header + 10..header + 12].copy_from_slice(&12u16.to_le_bytes()); // bzip2
    let err = package::read(&stored_then_patched).unwrap_err().to_string();
    assert!(err.contains("compression"), "{err}");
}

#[test]
fn first_install_trusts_the_publisher_key() {
    let key = Key::new(1);
    let bytes = plugin_zip(&key, &[]);
    let verified = verify(&bytes, &key.sign(&bytes), None).unwrap();
    assert_eq!(*verified.trust(), Trust::FirstInstall);
    assert_eq!(verified.key(), key.public());
}

#[test]
fn signatures_must_match_the_package_and_its_key() {
    let key = Key::new(1);
    let bytes = plugin_zip(&key, &[]);

    // Another key's signature over the same bytes.
    let other = Key::new(2);
    assert!(matches!(
        verify(&bytes, &other.sign(&bytes), None),
        Err(PackageError::Signature(_))
    ));

    // One byte changed after signing.
    let signature = key.sign(&bytes);
    // The first entry's modification time: still a valid, identical-looking
    // package, but not the bytes that were signed.
    let mut tampered = bytes.clone();
    tampered[10] ^= 1;
    assert!(package::read(&tampered).is_ok());
    assert!(matches!(
        verify(&tampered, &signature, None),
        Err(PackageError::Signature(_))
    ));

    // Garbage, and a signature file that's far too big.
    assert!(matches!(
        verify(&bytes, "not a signature", None),
        Err(PackageError::Signature(_))
    ));
    let huge = format!("{}\n", "a".repeat(10_000));
    assert!(matches!(
        verify(&bytes, &huge, None),
        Err(PackageError::Signature(_))
    ));
}

#[test]
fn later_packages_need_the_pinned_key() {
    let old = Key::new(1);
    let new = Key::new(2);

    let same = plugin_zip(&old, &[]);
    let verified = verify(&same, &old.sign(&same), Some(&old.public())).unwrap();
    assert_eq!(*verified.trust(), Trust::Pinned);

    // A validly signed package from someone else is refused outright.
    let takeover = plugin_zip(&new, &[]);
    match verify(&takeover, &new.sign(&takeover), Some(&old.public())) {
        Err(PackageError::KeyChanged { pinned, new: key }) => {
            assert_eq!(pinned, old.public());
            assert_eq!(key, new.public());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_pinned_key_can_endorse_a_new_one() {
    let old = Key::new(1);
    let new = Key::new(2);
    let statement = package::rotation_statement("nmu.test", &old.public(), &new.public());
    let endorsement = old.sign(statement.as_bytes());

    let bytes = plugin_zip(
        &new,
        &[
            ("rotation.txt", statement.as_bytes()),
            ("rotation.txt.minisig", endorsement.as_bytes()),
        ],
    );
    let verified = verify(&bytes, &new.sign(&bytes), Some(&old.public())).unwrap();
    assert_eq!(*verified.trust(), Trust::Rotated { from: old.public() });

    // Once rotated, the file can stay in later packages: the key matches
    // the pin, so it's ignored.
    let verified = verify(&bytes, &new.sign(&bytes), Some(&new.public())).unwrap();
    assert_eq!(*verified.trust(), Trust::Pinned);
}

#[test]
fn rotation_statements_are_checked_strictly() {
    let old = Key::new(1);
    let new = Key::new(2);
    let attacker = Key::new(3);

    let cases = [
        // Endorsed by the new key itself, not the pinned one.
        (
            package::rotation_statement("nmu.test", &old.public(), &new.public()),
            &new,
        ),
        // Another plugin's rotation, replayed.
        (
            package::rotation_statement("nmu.other", &old.public(), &new.public()),
            &old,
        ),
        // Endorses a different key than the one that signed the package.
        (
            package::rotation_statement("nmu.test", &old.public(), &attacker.public()),
            &old,
        ),
        // Extra text after the statement.
        (
            package::rotation_statement("nmu.test", &old.public(), &new.public())
                + "and also everything else\n",
            &old,
        ),
    ];
    for (statement, signer) in cases {
        let endorsement = signer.sign(statement.as_bytes());
        let bytes = plugin_zip(
            &new,
            &[
                ("rotation.txt", statement.as_bytes()),
                ("rotation.txt.minisig", endorsement.as_bytes()),
            ],
        );
        assert!(
            matches!(
                verify(&bytes, &new.sign(&bytes), Some(&old.public())),
                Err(PackageError::Rotation(_))
            ),
            "{statement}"
        );
    }
}

#[test]
fn an_archive_reads_only_one_way() {
    // Each of these could make the zip crate (or another zip tool) read a
    // different set of entries than the one Tether checked.
    let key = Key::new(1);
    let good = plugin_zip(&key, &[]);
    let end = good.len() - 22;
    assert!(package::read(&good).is_ok());

    let prepended = [b"junk before the archive".as_slice(), &good].concat();
    let mut comment_length = good.clone();
    comment_length[end + 20] = 1;
    let mut zip64 = good.clone();
    zip64[end + 16..end + 20].copy_from_slice(&u32::MAX.to_le_bytes());
    let stacked = [good.as_slice(), &good[end..]].concat();
    for (what, bytes) in [
        ("prepended data", prepended),
        ("a comment length", comment_length),
        ("a zip64 sentinel", zip64),
        ("a second end record", stacked),
    ] {
        assert!(
            matches!(package::read(&bytes), Err(PackageError::NotAZip(_))),
            "{what}"
        );
    }

    // The local header names another file than the central directory.
    let mut renamed = good.clone();
    let at = find(&renamed, b"plugin.wasm").unwrap();
    renamed[at + 10] = b'x';
    let err = package::read(&renamed).unwrap_err().to_string();
    assert!(err.contains("different names"), "{err}");
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn every_tool_sees_the_same_names() {
    // `notes.txt` in the headers, renamed to plugin.toml by an Info-ZIP
    // Unicode Path extra field that only some tools honour.
    let key = Key::new(1);
    let manifest = manifest("nmu.test", &key);
    // The writer checks the field's CRC against an empty name, so write it
    // with that CRC (0) and patch in the real one below.
    let mut unicode_path = vec![1u8, 0, 0, 0, 0];
    unicode_path.extend_from_slice(b"plugin.toml");
    let mut options = zip::write::FullFileOptions::default();
    options.add_extra_data(0x7075, unicode_path, false).unwrap();

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("notes.txt", options).unwrap();
    writer.write_all(manifest.as_bytes()).unwrap();
    writer
        .start_file("plugin.wasm", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(COMPONENT).unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], b"\x01\0\0\0\0plugin.toml") {
        let at = from + at;
        bytes[at + 1..at + 5].copy_from_slice(&crc32(b"notes.txt").to_le_bytes());
        from = at + 1;
    }

    let err = package::read(&bytes).unwrap_err().to_string();
    assert!(err.contains("different names"), "{err}");
}
