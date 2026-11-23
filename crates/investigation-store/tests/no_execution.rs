//! No code path starts a process, and no decompressor or archive reader is
//! linked. Artifact content and metadata are data: nothing in the shipped
//! sources can turn them into a command.
//!
//! The scan covers every Rust source of the shipped crates, unit tests
//! included. A process API may appear only in an entry of [`ALLOWED`], each of
//! which explains why it cannot run anything.
use std::{
    fs,
    path::{Path, PathBuf},
};

const CRATES: &[&str] = &[
    "investigation-core",
    "investigation-store",
    "investigation-recording",
    "investigation-render",
    "investigation-cli",
];

/// Text that names a way to start or replace a process, or to hand a file
/// to the desktop's default application.
const FORBIDDEN: &[&str] = &[
    "std::process",
    "process::Command",
    "Command::new",
    ".spawn(",
    "posix_spawn",
    "libc::exec",
    "libc::fork",
    "libc::system",
    "execvp",
    "execve",
    "CreateProcess",
    "ShellExecute",
    "xdg-open",
    "open::that",
    "opener::",
    "webbrowser::",
];

/// Uses that match [`FORBIDDEN`] but cannot start a process, by file and line
/// text, with the reason.
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "investigation-store/src/workspace.rs",
        "std::process::id()",
        "Reads this process's own ID to name a private temporary file.",
    ),
    (
        "investigation-store/src/crash_tests.rs",
        "std::process::id(),",
        "Unit test: names a temporary directory after this process's ID.",
    ),
    (
        "investigation-store/src/capture_items_tests.rs",
        "std::process::id(),",
        "Unit test: names a temporary directory after this process's ID.",
    ),
    (
        "investigation-store/src/durable.rs",
        "std::process::id()",
        "Unit test: names a temporary directory after this process's ID.",
    ),
    (
        "investigation-cli/src/shortcuts.rs",
        "Command::new(",
        "`clap::Command` builds argument-parser help for shortcut commands; it is not a process.",
    ),
    (
        "investigation-core/src/method_pack/tests.rs",
        "\"std::process\",",
        "Unit test: the forbidden-text list for the method-pack loader.",
    ),
    (
        "investigation-core/src/method_pack/tests.rs",
        "\"Command::new\",",
        "Unit test: the forbidden-text list for the method-pack loader.",
    ),
];

/// Crates that would decompress, unpack or launch something. None may be in
/// the locked dependency graph.
const FORBIDDEN_CRATES: &[&str] = &[
    "flate2",
    "miniz_oxide",
    "libz-sys",
    "libz-rs-sys",
    "zlib-rs",
    "zip",
    "tar",
    "zstd",
    "zstd-sys",
    "brotli",
    "brotli-decompressor",
    "xz2",
    "lzma-rs",
    "lzma-sys",
    "liblzma",
    "bzip2",
    "lz4",
    "lz4_flex",
    "snap",
    "async-compression",
    "sevenz-rust",
    "unrar",
    "open",
    "opener",
    "webbrowser",
    "duct",
    "subprocess",
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn sources(directory: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn shipped_sources_start_no_process() {
    let root = workspace().join("crates");
    let mut unexpected = Vec::new();
    let mut used = vec![false; ALLOWED.len()];
    let mut scanned = 0;
    for name in CRATES {
        let mut files = Vec::new();
        sources(&root.join(name).join("src"), &mut files);
        assert!(!files.is_empty(), "{name} has no sources");
        for file in files {
            scanned += 1;
            let relative = file
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let text = fs::read_to_string(&file).unwrap();
            for (number, line) in text.lines().enumerate() {
                if !FORBIDDEN.iter().any(|token| line.contains(token)) {
                    continue;
                }
                match ALLOWED
                    .iter()
                    .position(|(path, pattern, _)| relative == *path && line.contains(pattern))
                {
                    Some(index) => used[index] = true,
                    None => unexpected.push(format!("{relative}:{}: {}", number + 1, line.trim())),
                }
            }
        }
    }
    assert!(scanned > 100, "scanned only {scanned} files");
    assert!(
        unexpected.is_empty(),
        "process APIs outside the reviewed allowances:\n{}",
        unexpected.join("\n")
    );
    // A stale allowance would hide a later, different use at the same path.
    for ((path, pattern, _), used) in ALLOWED.iter().zip(used) {
        assert!(
            used,
            "allowance {path} {pattern:?} no longer matches; remove it"
        );
    }
    // The CLI's `Command` is clap's builder.
    let shortcuts = fs::read_to_string(root.join("investigation-cli/src/shortcuts.rs")).unwrap();
    assert!(shortcuts.contains("clap::") && shortcuts.contains("Command"));
}

#[test]
fn no_decompressor_archive_reader_or_launcher_is_linked() {
    let lock = fs::read_to_string(workspace().join("Cargo.lock")).unwrap();
    let names: Vec<&str> = lock
        .lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .collect();
    assert!(names.contains(&"serde_json"), "Cargo.lock was not parsed");
    let linked: Vec<_> = FORBIDDEN_CRATES
        .iter()
        .filter(|name| names.contains(name))
        .collect();
    assert!(
        linked.is_empty(),
        "{linked:?} would let the build expand or launch content; bound it in investigation_store::limits and add fixtures first"
    );
    const {
        assert!(!investigation_store::limits::EXPANSION.decompression);
        assert!(!investigation_store::limits::EXPANSION.archive_extraction);
    }
}
