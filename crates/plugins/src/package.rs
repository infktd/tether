//! Plugin packages: a `.zip` plus a detached minisign signature over it.
//!
//! [`read`] takes a package apart without trusting it: every entry name is
//! matched against a strict allowlist (never cleaned up and used anyway),
//! every read is capped whatever size the archive claims, and anything
//! unexpected (symlinks, encryption, duplicate names, other compression
//! methods, an archive comment) is refused.
//!
//! [`verify`] checks the signature and decides whether to trust the
//! signer: the first install pins the publisher key from `plugin.toml`;
//! later packages must be signed by the pinned key, or by a new key the
//! pinned one endorsed in a rotation statement shipped in the package.
//! Anything else needs an admin to re-pin the key by hand.
//!
//! The one exception is [`Unverified::into_bundled`]: the first-party
//! apps built into Tether's own image are exactly as trusted as the
//! binary, so they carry no signature and pin no key.

use std::io::{Cursor, Read};

use minisign_verify::{PublicKey, Signature};
use zip::{CompressionMethod, ZipArchive};

use crate::manifest::{Manifest, ManifestError};

/// The whole `.zip`.
pub const MAX_PACKAGE_BYTES: usize = 40 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 256;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_MIGRATIONS: usize = 100;
pub const MAX_MIGRATION_BYTES: usize = 256 * 1024;
pub const MAX_ASSETS: usize = 32;
pub const MAX_ASSET_BYTES: usize = 1024 * 1024;
/// A `.minisig` file, and the rotation statement.
pub const MAX_SIGNATURE_BYTES: usize = 4 * 1024;

pub const MANIFEST: &str = "plugin.toml";
pub const COMPONENT: &str = "plugin.wasm";
pub const ROTATION: &str = "rotation.txt";
pub const ROTATION_SIGNATURE: &str = "rotation.txt.minisig";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    #[error("the package is bigger than {MAX_PACKAGE_BYTES} bytes")]
    TooLarge,
    #[error("the package isn't a usable .zip: {0}")]
    NotAZip(String),
    #[error("{name}: {problem}")]
    Entry { name: String, problem: String },
    #[error("the package has no {0}")]
    Missing(&'static str),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("the signature doesn't match the package and its publisher key: {0}")]
    Signature(String),
    /// Signed by a key other than the pinned one, with no rotation
    /// statement from the pinned key. Only an admin re-pin gets past this.
    #[error("the package is signed with a different key than the one pinned for this plugin")]
    KeyChanged { pinned: String, new: String },
    #[error("the key rotation statement isn't valid: {0}")]
    Rotation(String),
    /// Only the apps bundled into Tether's image go without.
    #[error("plugin.toml has no [publisher] key, which a signed package needs")]
    NoPublisherKey,
}

fn entry(name: &str, problem: impl Into<String>) -> PackageError {
    PackageError::Entry {
        name: format!("{:?}", shown(name)),
        problem: problem.into(),
    }
}

/// An entry name for an error message: bounded, and printed escaped.
fn shown(name: &str) -> String {
    name.chars().take(80).collect()
}

/// A package's contents, read and checked, but not yet trusted.
#[derive(Clone, PartialEq, Eq)]
pub struct Package {
    pub manifest: Manifest,
    pub component: Vec<u8>,
    /// In order, numbered 1, 2, 3...
    pub migrations: Vec<Migration>,
    pub assets: Vec<Asset>,
    pub rotation: Option<RotationFiles>,
}

impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package")
            .field("id", &self.manifest.plugin.id)
            .field("version", &self.manifest.plugin.version)
            .field("component_bytes", &self.component.len())
            .field("migrations", &self.migrations.len())
            .field("assets", &self.assets.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub version: u32,
    /// From the file name, e.g. `create_ledger` for `0001_create_ledger.sql`.
    pub name: String,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The file name under `ui/`, e.g. `icon.png`.
    pub name: String,
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// `rotation.txt` and its signature, as shipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationFiles {
    pub statement: String,
    pub signature: String,
}

/// What an entry is, by name alone.
enum Kind {
    Manifest,
    Component,
    Migration {
        version: u32,
        name: String,
    },
    Asset {
        content_type: &'static str,
    },
    Rotation,
    RotationSignature,
    /// `migrations/` or `ui/`, which zip tools add; skipped.
    Directory,
}

fn kind(name: &[u8]) -> Option<Kind> {
    // Names are ASCII by construction below, so no encoding questions.
    let name = std::str::from_utf8(name).ok()?;
    match name {
        MANIFEST => return Some(Kind::Manifest),
        COMPONENT => return Some(Kind::Component),
        ROTATION => return Some(Kind::Rotation),
        ROTATION_SIGNATURE => return Some(Kind::RotationSignature),
        "migrations/" | "ui/" => return Some(Kind::Directory),
        _ => {}
    }
    let lower = |s: &str, extra: &[u8]| {
        (1..=60).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || extra.contains(&b))
    };
    if let Some(file) = name.strip_prefix("migrations/") {
        // 0001_create_ledger.sql
        let stem = file.strip_suffix(".sql")?;
        let (number, label) = stem.split_once('_')?;
        if number.len() != 4 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if !lower(label, b"_") {
            return None;
        }
        return Some(Kind::Migration {
            version: number.parse().ok()?,
            name: label.to_owned(),
        });
    }
    if let Some(file) = name.strip_prefix("ui/") {
        let (stem, extension) = file.rsplit_once('.')?;
        if !lower(stem, b"_-") {
            return None;
        }
        let content_type = match extension {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => return None,
        };
        return Some(Kind::Asset { content_type });
    }
    None
}

/// Whether the bytes are really the image type the name says: assets are
/// served with that type, and browsers shouldn't have to guess.
fn image_matches(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

/// Where the central directory starts and how many entries it has, from
/// the end-of-central-directory record, which must be the file's last 22
/// bytes: no archive comment, no zip64, one disk, and the directory right
/// before it. That leaves exactly one way to read the archive, and
/// [`read`] then checks the zip crate read it that way (it would otherwise
/// fall back to an earlier record, or follow a zip64 locator). The zip
/// crate keys entries by name, so a repeated name silently replaces the
/// earlier one; comparing entry counts is how duplicates show up.
fn central_directory(bytes: &[u8]) -> Result<(u64, usize), PackageError> {
    const EOCD_LEN: usize = 22;
    let not_zip = |why: &str| PackageError::NotAZip(why.to_owned());
    let start = bytes
        .len()
        .checked_sub(EOCD_LEN)
        .ok_or_else(|| not_zip("too short"))?;
    let eocd = &bytes[start..];
    if eocd[..4] != [0x50, 0x4b, 0x05, 0x06] || eocd[20..22] != [0, 0] {
        return Err(not_zip(
            "it must end with the end-of-central-directory record (no archive comment)",
        ));
    }
    let u16_at = |at: usize| u16::from_le_bytes([eocd[at], eocd[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes([eocd[at], eocd[at + 1], eocd[at + 2], eocd[at + 3]]);
    let (disk, cd_disk, on_disk, total) = (u16_at(4), u16_at(6), u16_at(8), u16_at(10));
    let (cd_size, cd_offset) = (u32_at(12), u32_at(16));
    if disk != 0 || cd_disk != 0 || on_disk != total {
        return Err(not_zip("multi-part archives aren't supported"));
    }
    if total == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX {
        return Err(not_zip("zip64 archives aren't supported"));
    }
    if u64::from(cd_offset) + u64::from(cd_size) != start as u64 {
        return Err(not_zip(
            "the central directory must come right before its end record",
        ));
    }
    Ok((u64::from(cd_offset), usize::from(total)))
}

/// The file name stored in a local (`PK\x03\x04`, name at 30) or central
/// (`PK\x01\x02`, name at 46) header. Both must match the name the zip
/// crate reports: other tools read the local one, and the crate would
/// otherwise take a name from a Unicode Path extra field instead, so a
/// package must look the same to every tool as it does to Tether.
fn stored_name(bytes: &[u8], start: u64, central: bool) -> Option<&[u8]> {
    let (signature, length_at, name_at) = if central {
        ([0x50, 0x4b, 0x01, 0x02], 28, 46)
    } else {
        ([0x50, 0x4b, 0x03, 0x04], 26, 30)
    };
    let at = usize::try_from(start).ok()?;
    let header = bytes.get(at..at.checked_add(name_at)?)?;
    if header[..4] != signature {
        return None;
    }
    let length = usize::from(u16::from_le_bytes([
        header[length_at],
        header[length_at + 1],
    ]));
    bytes.get(at + name_at..at + name_at + length)
}

/// Reads at most `cap` bytes of an entry, failing if there are more.
fn read_capped(reader: impl Read, name: &str, cap: usize) -> Result<Vec<u8>, PackageError> {
    let mut bytes = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| entry(name, format!("can't be read: {e}")))?;
    if bytes.len() > cap {
        return Err(entry(name, format!("is bigger than {cap} bytes")));
    }
    Ok(bytes)
}

fn utf8(bytes: Vec<u8>, name: &str) -> Result<String, PackageError> {
    String::from_utf8(bytes).map_err(|_| entry(name, "isn't UTF-8 text"))
}

/// A package taken apart and checked, except for its signature.
pub struct Unverified<'a> {
    package: Package,
    bytes: &'a [u8],
}

impl std::fmt::Debug for Unverified<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.package.fmt(f)
    }
}

impl Unverified<'_> {
    /// The plugin id it claims: look up the pinned key by it.
    pub fn id(&self) -> &str {
        &self.package.manifest.plugin.id
    }

    /// What it contains. Nothing here is trusted yet.
    pub fn package(&self) -> &Package {
        &self.package
    }

    /// Checks the signature against the package's publisher key, and that
    /// key against `pinned`, the key pinned for [`Self::id`] (`None` on
    /// first install).
    pub fn verify(self, signature: &str, pinned: Option<&str>) -> Result<Verified, PackageError> {
        let package = self.package;
        let key = package
            .manifest
            .publisher
            .as_ref()
            .ok_or(PackageError::NoPublisherKey)?
            .key
            .clone();
        let key = key.as_str();
        check_signature(key, self.bytes, signature).map_err(PackageError::Signature)?;

        let trust = match pinned {
            None => Trust::FirstInstall,
            Some(pinned) if pinned == key => Trust::Pinned,
            Some(pinned) => {
                let Some(rotation) = &package.rotation else {
                    return Err(PackageError::KeyChanged {
                        pinned: pinned.to_owned(),
                        new: key.to_owned(),
                    });
                };
                let expected = rotation_statement(&package.manifest.plugin.id, pinned, key);
                if rotation.statement != expected {
                    return Err(PackageError::Rotation(format!(
                        "{ROTATION} must say exactly that the pinned key endorses the package's \
                         key for this plugin"
                    )));
                }
                check_signature(pinned, rotation.statement.as_bytes(), &rotation.signature)
                    .map_err(|e| {
                        PackageError::Rotation(format!("not signed by the pinned key: {e}"))
                    })?;
                Trust::Rotated {
                    from: pinned.to_owned(),
                }
            }
        };
        Ok(Verified {
            key: key.to_owned(),
            package,
            trust,
        })
    }

    /// The package as one bundled into Tether's own image: trusted as the
    /// binary is, with no signature and no key. Only for packages read
    /// from the image's bundled apps directory, never from an upload or a
    /// download.
    pub fn into_bundled(self) -> Package {
        self.package
    }
}

/// Takes a package apart and checks everything in it except the signature,
/// which [`Unverified::verify`] checks next.
pub fn read(bytes: &[u8]) -> Result<Unverified<'_>, PackageError> {
    let package = read_package(bytes)?;
    Ok(Unverified { package, bytes })
}

fn read_package(bytes: &[u8]) -> Result<Package, PackageError> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(PackageError::TooLarge);
    }
    let (directory_start, declared) = central_directory(bytes)?;
    if declared > MAX_ENTRIES {
        return Err(PackageError::NotAZip(format!(
            "it has more than {MAX_ENTRIES} entries"
        )));
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| PackageError::NotAZip(e.to_string()))?;
    if archive.offset() != 0 || archive.central_directory_start() != directory_start {
        return Err(PackageError::NotAZip(
            "it has data before the archive, or more than one central directory".to_owned(),
        ));
    }
    if archive.len() != declared {
        return Err(PackageError::NotAZip(
            "an entry name appears more than once".to_owned(),
        ));
    }

    let mut manifest = None;
    let mut component = None;
    let mut migrations = Vec::new();
    let mut assets = Vec::new();
    let mut rotation = None;
    let mut rotation_signature = None;

    for index in 0..archive.len() {
        // Metadata first, without decompressing anything.
        let (kind, name, cap) = {
            let file = archive
                .by_index_raw(index)
                .map_err(|e| PackageError::NotAZip(e.to_string()))?;
            let name = String::from_utf8_lossy(file.name_raw()).into_owned();
            let kind = kind(file.name_raw())
                .ok_or_else(|| entry(&name, "isn't a file a package may contain"))?;
            if stored_name(bytes, file.header_start(), false) != Some(file.name_raw())
                || stored_name(bytes, file.central_header_start(), true) != Some(file.name_raw())
            {
                return Err(entry(&name, "has different names in its headers"));
            }
            if file.encrypted() {
                return Err(entry(&name, "is encrypted"));
            }
            if file.is_symlink() {
                return Err(entry(&name, "is a symbolic link"));
            }
            if !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ) {
                return Err(entry(&name, "uses a compression method other than deflate"));
            }
            let cap = match kind {
                Kind::Manifest => MAX_MANIFEST_BYTES,
                Kind::Component => crate::MAX_COMPONENT_BYTES,
                Kind::Migration { .. } => MAX_MIGRATION_BYTES,
                Kind::Asset { .. } => MAX_ASSET_BYTES,
                Kind::Rotation | Kind::RotationSignature => MAX_SIGNATURE_BYTES,
                Kind::Directory => 0,
            };
            // What the archive claims; the capped read below is what counts.
            if file.size() > cap as u64 {
                return Err(entry(&name, format!("is bigger than {cap} bytes")));
            }
            (kind, name, cap)
        };
        let file = archive
            .by_index(index)
            .map_err(|e| entry(&name, e.to_string()))?;
        let data = read_capped(file, &name, cap)?;
        match kind {
            Kind::Directory => {}
            Kind::Manifest => manifest = Some(utf8(data, &name)?),
            Kind::Component => component = Some(data),
            Kind::Migration {
                version,
                name: label,
            } => {
                if migrations.len() == MAX_MIGRATIONS {
                    return Err(PackageError::NotAZip(format!(
                        "it has more than {MAX_MIGRATIONS} migrations"
                    )));
                }
                migrations.push(Migration {
                    version,
                    name: label,
                    sql: utf8(data, &name)?,
                });
            }
            Kind::Asset { content_type } => {
                if assets.len() == MAX_ASSETS {
                    return Err(PackageError::NotAZip(format!(
                        "it has more than {MAX_ASSETS} ui files"
                    )));
                }
                if !image_matches(content_type, &data) {
                    return Err(entry(&name, format!("isn't a {content_type} image")));
                }
                let file = name.strip_prefix("ui/").unwrap_or(&name).to_owned();
                assets.push(Asset {
                    name: file,
                    content_type,
                    bytes: data,
                });
            }
            Kind::Rotation => rotation = Some(utf8(data, &name)?),
            Kind::RotationSignature => rotation_signature = Some(utf8(data, &name)?),
        }
    }

    let manifest = Manifest::parse(&manifest.ok_or(PackageError::Missing(MANIFEST))?)?;
    let component = component.ok_or(PackageError::Missing(COMPONENT))?;

    migrations.sort_by_key(|m| m.version);
    for (expected, migration) in (1..).zip(&migrations) {
        if migration.version != expected {
            return Err(PackageError::NotAZip(format!(
                "migrations must be numbered 0001, 0002, ... with no gaps or repeats; \
                 expected {expected:04}, found {:04}",
                migration.version
            )));
        }
    }
    assets.sort_by(|a, b| a.name.cmp(&b.name));

    let rotation = match (rotation, rotation_signature) {
        (None, None) => None,
        (Some(statement), Some(signature)) => Some(RotationFiles {
            statement,
            signature,
        }),
        _ => {
            return Err(PackageError::Rotation(format!(
                "{ROTATION} and {ROTATION_SIGNATURE} come together"
            )));
        }
    };

    Ok(Package {
        manifest,
        component,
        migrations,
        assets,
        rotation,
    })
}

/// Why a verified package is trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// No key is pinned for this plugin yet; installing pins this one.
    FirstInstall,
    /// Signed by the pinned key.
    Pinned,
    /// Signed by a new key the pinned one endorsed; installing replaces
    /// the pin.
    Rotated { from: String },
}

/// A package whose signature checks out, and why its key is trusted. Only
/// [`Unverified::verify`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    package: Package,
    trust: Trust,
    key: String,
}

impl Verified {
    pub fn package(&self) -> &Package {
        &self.package
    }

    pub fn trust(&self) -> &Trust {
        &self.trust
    }

    /// The publisher key that signed it (and is pinned once installed).
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn into_package(self) -> Package {
        self.package
    }
}

/// The exact text a pinned key signs to endorse a new one.
pub fn rotation_statement(plugin_id: &str, old_key: &str, new_key: &str) -> String {
    format!("tether-key-rotation v1\nplugin: {plugin_id}\nold: {old_key}\nnew: {new_key}\n")
}

fn check_signature(key: &str, data: &[u8], signature: &str) -> Result<(), String> {
    if signature.len() > MAX_SIGNATURE_BYTES {
        return Err("the signature file is too big".to_owned());
    }
    let key = PublicKey::from_base64(key).map_err(|e| e.to_string())?;
    let signature = Signature::decode(signature).map_err(|e| e.to_string())?;
    // Only prehashed (`ED`) signatures: legacy ones hash nothing, which is
    // slow on large files and has no benefit.
    key.verify(data, &signature, false)
        .map_err(|e| e.to_string())
}
