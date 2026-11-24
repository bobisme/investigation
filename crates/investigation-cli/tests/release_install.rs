//! The release package's install script (REL.04), run against this build's
//! binary in a package directory laid out as `scripts/package-release.py`
//! lays it out: it installs `investigation` into the prefix it is given,
//! keeps an unrelated `inv` on PATH and an `inv` already in the prefix byte
//! for byte, adds the optional `inv` alias only where no `inv` exists,
//! refuses a second install without `--replace`, and installs nothing when a
//! packaged file does not match SHA256SUMS. The full package, built twice and
//! smoke-tested in a clean root, is `just package` and `just smoke-package`.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use investigation_store::journal::hash_bytes;

const UNRELATED_INV: &[u8] = b"#!/bin/sh\necho 'an unrelated inv program'\n";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn executable(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A package directory holding this build's binary and the install script.
fn package(root: &Path) -> PathBuf {
    let package = root.join("package");
    executable(
        &package.join("bin/investigation"),
        &fs::read(env!("CARGO_BIN_EXE_investigation")).unwrap(),
    );
    executable(
        &package.join("install.sh"),
        &fs::read(repository().join("scripts/release/install.sh")).unwrap(),
    );
    fs::copy(repository().join("LICENSE"), package.join("LICENSE")).unwrap();
    let sums: String = ["LICENSE", "bin/investigation", "install.sh"]
        .iter()
        .map(|name| {
            let digest = hash_bytes(&fs::read(package.join(name)).unwrap());
            format!("{}  {name}\n", &digest.as_str()["sha256:".len()..])
        })
        .collect();
    fs::write(package.join("SHA256SUMS"), sums).unwrap();
    package
}

/// `install.sh ARGS` with PATH limited to `path` and the system tools.
fn install(package: &Path, path: &str, args: &[&str]) -> (bool, String) {
    let output = Command::new(package.join("install.sh"))
        .args(args)
        .env_clear()
        .env("PATH", format!("{path}:/usr/bin:/bin"))
        .current_dir(package.parent().unwrap())
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr),
    )
}

fn resolve_inv(path: &str) -> String {
    let output = Command::new("/bin/sh")
        .args(["-c", "command -v inv"])
        .env_clear()
        .env("PATH", format!("{path}:/usr/bin:/bin"))
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn install_keeps_every_existing_inv_and_adds_the_alias_only_where_none_exists() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let package = package(root);
    let built = fs::read(package.join("bin/investigation")).unwrap();

    // An unrelated `inv` earlier on PATH stays the one PATH finds.
    let other = root.join("other/bin");
    executable(&other.join("inv"), UNRELATED_INV);
    let prefix = root.join("prefix");
    let path = format!("{}:{}", other.display(), prefix.join("bin").display());
    let (ok, output) = install(
        &package,
        &path,
        &["--prefix", prefix.to_str().unwrap(), "--alias-inv"],
    );
    assert!(ok, "{output}");
    assert!(output.contains("kept the existing inv on PATH"), "{output}");
    assert_eq!(fs::read(prefix.join("bin/investigation")).unwrap(), built);
    assert!(fs::symlink_metadata(prefix.join("bin/inv")).is_err());
    assert_eq!(fs::read(other.join("inv")).unwrap(), UNRELATED_INV);
    assert_eq!(resolve_inv(&path), other.join("inv").to_str().unwrap());

    // A second install is refused without --replace and changes nothing.
    let (ok, output) = install(&package, &path, &["--prefix", prefix.to_str().unwrap()]);
    assert!(!ok && output.contains("--replace"), "{output}");
    let (ok, output) = install(
        &package,
        &path,
        &["--prefix", prefix.to_str().unwrap(), "--replace"],
    );
    assert!(ok, "{output}");

    // An `inv` already in the prefix is kept.
    let kept = root.join("kept");
    executable(&kept.join("bin/inv"), UNRELATED_INV);
    let (ok, output) = install(
        &package,
        kept.join("bin").to_str().unwrap(),
        &["--prefix", kept.to_str().unwrap(), "--alias-inv"],
    );
    assert!(ok, "{output}");
    assert!(output.contains("kept the existing"), "{output}");
    let metadata = fs::symlink_metadata(kept.join("bin/inv")).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(fs::read(kept.join("bin/inv")).unwrap(), UNRELATED_INV);

    // With no `inv` anywhere the alias is added, as a link to investigation.
    // (A system-wide `inv` would rightly block it; the case is then skipped.)
    if resolve_inv("").is_empty() {
        let free = root.join("free");
        let (ok, output) = install(
            &package,
            free.join("bin").to_str().unwrap(),
            &["--prefix", free.to_str().unwrap(), "--alias-inv"],
        );
        assert!(ok, "{output}");
        assert_eq!(
            fs::read_link(free.join("bin/inv")).unwrap(),
            Path::new("investigation")
        );
    }

    // A packaged file that does not match SHA256SUMS installs nothing.
    let mut changed = built.clone();
    changed.push(0);
    fs::write(package.join("bin/investigation"), changed).unwrap();
    let refused = root.join("refused");
    let (ok, output) = install(&package, "", &["--prefix", refused.to_str().unwrap()]);
    assert!(!ok && output.contains("SHA256SUMS"), "{output}");
    assert!(!refused.exists());
}
