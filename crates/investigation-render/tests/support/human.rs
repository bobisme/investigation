//! Human-owned areas for the renderer's tests: seed them with awkward
//! content and take a fingerprint that changes if anything inside is
//! written, replaced, moved, created or deleted, even briefly.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::{Path, PathBuf},
};

use investigation_render::layout;

/// What one entry under a human-owned folder looks like: its type, inode,
/// modification time and bytes (a link's target). A directory's modification
/// time changes whenever an entry is created, renamed or removed in it, so a
/// temporary created and removed again is caught too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub kind: &'static str,
    pub inode: u64,
    pub modified: (i64, i64),
    pub bytes: Vec<u8>,
}

/// Every entry in the vault's human-owned top-level folders, folders
/// included, by vault-relative path.
pub fn fingerprint(vault: &Path) -> BTreeMap<PathBuf, Seen> {
    let mut found = BTreeMap::new();
    let mut pending: Vec<PathBuf> = fs::read_dir(vault)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            layout::is_human_root(name) && fs::symlink_metadata(path).unwrap().is_dir()
        })
        .collect();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let (kind, bytes) = if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            ("link", target.into_os_string().into_encoded_bytes())
        } else if metadata.is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
            ("folder", Vec::new())
        } else {
            ("file", fs::read(&path).unwrap())
        };
        found.insert(
            path.strip_prefix(vault).unwrap().to_owned(),
            Seen {
                kind,
                inode: metadata.ino(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                bytes,
            },
        );
    }
    found
}

/// Fill `Notes/`, `Boards/`, `Inbox/` and a viewer's settings folder with
/// content a careless publisher might touch: names shaped like the
/// publisher's temporaries and like conflict copies, a note with an `inv_id`
/// property, a link, an empty folder and hidden files.
pub fn seed(vault: &Path) {
    let files: &[(&str, &[u8])] = &[
        ("Notes/thinking.md", b"my own analysis\n"),
        (
            "Notes/deep/nested/idea.md",
            b"---\ninv_id: q_x\n---\nnot generated\n",
        ),
        (
            "Notes/.thinking.md.render-0.inv-del",
            b"looks like an aside\n",
        ),
        (
            "Notes/.idea.md.render-0.inv-tmp",
            b"looks like a temporary\n",
        ),
        (
            "Notes/thinking (conflicted copy).md",
            b"a sync copy of mine\n",
        ),
        ("Boards/plan.canvas", b"{\"nodes\":[],\"edges\":[]}\n"),
        ("Inbox/clipping.txt", b"candidate material\n"),
        ("Inbox/.hidden", b"hidden\n"),
        (".obsidian/app.json", b"{}\n"),
    ];
    for (path, bytes) in files {
        let path = vault.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fs::create_dir_all(vault.join("Boards/empty")).unwrap();
    symlink("../Cases", vault.join("Notes/cases-link")).unwrap();
}

/// Copy a generated note's bytes to the same plan path under `Notes/` and
/// `Inbox/`: files a careless publisher might adopt or retire as its own.
pub fn seed_mirror(vault: &Path, generated: &str, bytes: &[u8]) {
    for root in ["Notes", "Inbox"] {
        let path = vault.join(root).join(generated);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}
