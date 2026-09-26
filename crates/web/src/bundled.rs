//! Apps bundled into Tether's image: the first-party apps (`plugins/` in
//! the repository), built and packaged by the image build into
//! [`DEFAULT_DIR`], so every deployment has them without anyone signing
//! anything.
//!
//! A bundled package is exactly as trusted as the binary, since it ships
//! in the same image: it has no signature and pins no key. It is still
//! reviewed and approved like any other install (what it asks for, its
//! permissions, hosts, secrets and scopes), from "Included with Tether" on
//! the Apps page. A newer image carrying a newer version shows it as an
//! update, reviewed and rolled back like any other.
//!
//! The ids of bundled apps are reserved: no package from anywhere else
//! (a file, GitHub) can install or upgrade them.

use std::collections::BTreeMap;
use std::path::Path;

use tether_plugins::package::{self, Package};

/// Where the app image puts them.
pub const DEFAULT_DIR: &str = "/usr/share/tether/apps";

/// One bundled package, read and checked (everything but a signature,
/// which it doesn't have).
pub struct BundledApp {
    pub package: Package,
    /// The package as shipped: what is stored on install.
    pub bytes: Vec<u8>,
    pub sha256: Vec<u8>,
}

impl std::fmt::Debug for BundledApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.package.fmt(f)
    }
}

/// The bundled apps this Tether ships with, by id. Read once at startup.
#[derive(Debug, Default)]
pub struct Bundled {
    apps: BTreeMap<String, BundledApp>,
}

impl Bundled {
    /// No bundled apps (tests, or a development build without them).
    pub fn none() -> Self {
        Self::default()
    }

    /// Reads every `*.zip` in `dir`. A missing directory means none (a
    /// development build); a package that doesn't read is left out and
    /// logged, and so is a second package for the same id.
    pub fn read_dir(dir: &Path) -> Self {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(dir = %dir.display(), "no bundled apps directory");
                return Self::none();
            }
            Err(e) => {
                tracing::error!(dir = %dir.display(), error = %e, "reading the bundled apps");
                return Self::none();
            }
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "zip"))
            .collect();
        paths.sort();
        let mut bundled = Self::none();
        for path in paths {
            match read_one(&path) {
                Ok(app) => bundled.add(app, &path),
                Err(why) => {
                    tracing::error!(path = %path.display(), error = why, "a bundled app can't be read");
                }
            }
        }
        tracing::info!(
            apps = bundled.apps.len(),
            dir = %dir.display(),
            "bundled apps read"
        );
        bundled
    }

    /// From packages already in memory (tests).
    pub fn from_packages(packages: Vec<Vec<u8>>) -> Result<Self, package::PackageError> {
        let mut bundled = Self::none();
        for bytes in packages {
            let package = package::read(&bytes)?.into_bundled();
            bundled.add(
                BundledApp {
                    sha256: crate::plugins::sha256(&bytes),
                    package,
                    bytes,
                },
                Path::new("(memory)"),
            );
        }
        Ok(bundled)
    }

    fn add(&mut self, app: BundledApp, path: &Path) {
        let id = app.package.manifest.plugin.id.clone();
        if self.apps.contains_key(&id) {
            tracing::error!(
                plugin = id,
                path = %path.display(),
                "another bundled package has this app's id; only the first is used"
            );
            return;
        }
        self.apps.insert(id, app);
    }

    pub fn get(&self, id: &str) -> Option<&BundledApp> {
        self.apps.get(id)
    }

    /// Whether `id` is a bundled app's, so nothing else may install it.
    pub fn reserves(&self, id: &str) -> bool {
        self.apps.contains_key(id)
    }

    /// Every bundled app, by name.
    pub fn all(&self) -> Vec<&BundledApp> {
        let mut apps: Vec<&BundledApp> = self.apps.values().collect();
        apps.sort_by(|a, b| {
            a.package
                .manifest
                .plugin
                .name
                .cmp(&b.package.manifest.plugin.name)
        });
        apps
    }
}

fn read_one(path: &Path) -> Result<BundledApp, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_owned());
    }
    if meta.len() > package::MAX_PACKAGE_BYTES as u64 {
        return Err(format!("bigger than {} bytes", package::MAX_PACKAGE_BYTES));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let package = package::read(&bytes)
        .map_err(|e| e.to_string())?
        .into_bundled();
    Ok(BundledApp {
        sha256: crate::plugins::sha256(&bytes),
        package,
        bytes,
    })
}
