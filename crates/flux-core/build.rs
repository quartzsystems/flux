//! Compiles the appliance version into the binaries.
//!
//! The version lives in one place — the `VERSION` file at the repository root —
//! and everything else derives from it. Cargo cannot read a file into a manifest,
//! so `Cargo.toml` and `web/package.json` are kept in step by
//! `scripts/sync-version.sh` and checked in CI. The value the running appliance
//! reports, however, comes straight from the file, which means a binary can never
//! disagree with the tree it was built from.

use std::path::{Path, PathBuf};

fn main() {
    let path = version_file();
    println!("cargo:rerun-if-changed={}", path.display());

    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading the version from {}: {e}", path.display()));
    let version = raw.trim();

    assert!(!version.is_empty(), "{} is empty; it must hold a version like 0.1.0", path.display());
    assert!(
        !version.contains(['\n', '"']),
        "{} must hold a single bare version, got {version:?}",
        path.display()
    );

    println!("cargo:rustc-env=FLUX_VERSION={version}");
}

/// Locates `VERSION` by walking up from this crate.
///
/// Walking rather than hard-coding `../../VERSION` so the build does not break if
/// the crate is ever moved to a different depth — a wrong path here fails at
/// build time with a confusing message about a missing file.
fn version_file() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by cargo for every build script");

    let mut dir: Option<&Path> = Some(Path::new(&manifest));
    while let Some(current) = dir {
        let candidate = current.join("VERSION");
        if candidate.is_file() {
            return candidate;
        }
        dir = current.parent();
    }

    panic!("no VERSION file found in {manifest} or any parent directory");
}
