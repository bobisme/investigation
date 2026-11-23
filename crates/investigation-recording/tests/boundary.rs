//! Source-level guards for the recording crate's boundary promises.
//! `scripts/check-boundaries.py` also checks the full resolved dependency tree.

use std::{fs, path::Path};

const CRATE: &str = env!("CARGO_MANIFEST_DIR");

fn sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(Path::new(CRATE).join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            files.push((
                path.display().to_string(),
                fs::read_to_string(&path).unwrap(),
            ));
        }
    }
    assert!(!files.is_empty());
    files
}

#[test]
fn the_library_never_spawns_processes_or_loads_code() {
    for (path, source) in sources() {
        for forbidden in [
            "std::process",
            "process::Command",
            "Command::new",
            "libloading",
            "dlopen",
        ] {
            assert!(!source.contains(forbidden), "{path} contains {forbidden:?}");
        }
    }
}

#[test]
fn the_manifest_names_no_frontend_crate() {
    let manifest = fs::read_to_string(Path::new(CRATE).join("Cargo.toml")).unwrap();
    for frontend in ["investigation-cli", "investigation-mcp"] {
        assert!(
            !manifest.contains(frontend),
            "Cargo.toml depends on {frontend}"
        );
    }
}
