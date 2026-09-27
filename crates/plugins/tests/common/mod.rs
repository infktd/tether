//! Building test guests for the runtime's tests.

use std::path::PathBuf;

/// Builds a workspace crate for wasm32-wasip2 into its own target
/// directory (so it doesn't wait on the lock of the build running us) and
/// returns the component.
pub fn build_guest(package: &str) -> Vec<u8> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target/test-guests");
    let _lock = guests_lock(&root);
    let output = std::process::Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "build",
            "-p",
            package,
            "--target",
            "wasm32-wasip2",
            "--release",
        ])
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .expect("running cargo");
    assert!(
        output.status.success(),
        "building {package} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file = format!("wasm32-wasip2/release/{}.wasm", package.replace('-', "_"));
    std::fs::read(target.join(file)).expect("reading the test guest")
}

/// Holds `target/test-guests.lock` while building and reading guests. Test
/// processes run in parallel (nextest), and a build another one starts
/// (other features, the bundled apps) can replace a guest's file while
/// this one reads it.
pub fn guests_lock(root: &std::path::Path) -> std::fs::File {
    let target = root.join("target");
    std::fs::create_dir_all(&target).expect("creating target/");
    let lock = std::fs::File::create(target.join("test-guests.lock")).expect("opening the lock");
    lock.lock().expect("locking the test guests");
    lock
}
