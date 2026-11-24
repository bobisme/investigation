//! Disposable verdicts of retained blobs that were hashed (MB.06, §10.5).
//!
//! Read paths (`show`, `brief`, `explain`, `search` and every other view
//! of effective state) report whether the bytes an Artifact cites are still
//! intact. Hashing every cited blob on every read costs time proportional to
//! the retained bytes, so whenever a blob is fully hashed (published by an
//! ingest, hashed by a read that found no verdict, by `evidence read`, by
//! `check` or by a promotion) the result is kept under
//! `cache/blob-verdicts/`, with the identity of the file that was hashed
//! (`DurableFs::identity`: device, inode, size, modification and change
//! times). A read reuses a verdict only for a file of that same identity;
//! any write, truncation, replacement or mode change of the blob changes the
//! identity, and the next read hashes again.
//!
//! The cache is never canonical. An absent, unreadable or unparsable entry
//! means "hash again", and promotion gates and `check` never consult it.
//! A verdict is recorded only when the identity is the same before and after
//! the hash, and only for a definite result: an I/O error records nothing.
//! It is not a defence against a writer of the workspace, who can change the
//! canonical files as easily; that is what `check` and the gates are for.

use super::*;

/// Where verdicts live, one file per blob digest.
const DIRECTORY: &str = "cache/blob-verdicts";
/// The first word of an entry. A different word is a miss.
const FORMAT: &str = "inv.blob-verdict.v1";

fn entry_path(hex: &str) -> Option<PathBuf> {
    let valid = hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    valid.then(|| Path::new(DIRECTORY).join(hex))
}

fn entry(size: u64, identity: &FileIdentity, intact: bool) -> String {
    let verdict = if intact { "intact" } else { "corrupt" };
    format!("{FORMAT} {size} {} {verdict}\n", identity.key())
}

fn parse(bytes: &[u8], size: u64, identity: &FileIdentity) -> Option<bool> {
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
    let mut words = text.split(' ');
    let format = words.next()?;
    let recorded = words.next()?.parse::<u64>().ok()?;
    let key = words.next()?;
    let verdict = match words.next()? {
        "intact" => true,
        "corrupt" => false,
        _ => return None,
    };
    (format == FORMAT && recorded == size && key == identity.key() && words.next().is_none())
        .then_some(verdict)
}

impl<F: DurableFs> Store<F> {
    /// The recorded verdict on the blob `hex` claimed at `size` bytes, when
    /// it was taken of the file whose identity is `current`.
    pub(crate) fn cached_verdict(
        &self,
        hex: &str,
        size: u64,
        current: &FileIdentity,
    ) -> Option<bool> {
        let bytes = self.fs.read_cache(&entry_path(hex)?)?;
        parse(&bytes, size, current)
    }

    /// Keep the verdict of a hash of the blob `hex` claimed at `size`
    /// bytes. `before` is the identity seen just before the bytes were
    /// opened; nothing is kept unless the file still has it.
    pub(crate) fn record_verdict(&self, hex: &str, size: u64, before: &FileIdentity, intact: bool) {
        let Some(path) = entry_path(hex) else {
            return;
        };
        let blob = Path::new(BLOBS).join(&hex[..2]).join(hex);
        if self.fs.identity(&blob).ok().as_ref() == Some(before) {
            let entry = entry(size, before, intact);
            // A read that confirms a kept verdict writes nothing (W11.15).
            if self.fs.read_cache(&path).as_deref() != Some(entry.as_bytes()) {
                self.fs.write_cache(&path, entry.as_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable::LocalFs;

    fn identity_of(fs: &LocalFs, path: &Path) -> FileIdentity {
        fs.identity(path).unwrap()
    }

    #[test]
    fn a_verdict_is_reused_only_for_the_same_file_and_claim() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let hex = "ab".repeat(32);
        let blob = Path::new(BLOBS).join(&hex[..2]).join(&hex);
        std::fs::create_dir_all(root.join(blob.parent().unwrap())).unwrap();
        std::fs::write(root.join(&blob), b"bytes").unwrap();
        let fs = LocalFs::new(root);
        let identity = identity_of(&fs, &blob);
        let line = entry(5, &identity, true);
        assert_eq!(parse(line.as_bytes(), 5, &identity), Some(true));
        // Another claimed size, a truncated entry or another format: a miss.
        assert_eq!(parse(line.as_bytes(), 6, &identity), None);
        assert_eq!(
            parse(&line.as_bytes()[..line.len() - 2], 5, &identity),
            None
        );
        assert_eq!(
            parse(
                line.replace(FORMAT, "inv.blob-verdict.v0").as_bytes(),
                5,
                &identity
            ),
            None
        );
        assert_eq!(
            parse(entry(5, &identity, false).as_bytes(), 5, &identity),
            Some(false)
        );
        // A replaced file has another identity, so the entry no longer applies.
        std::fs::remove_file(root.join(&blob)).unwrap();
        std::fs::write(root.join(&blob), b"other").unwrap();
        let replaced = identity_of(&fs, &blob);
        assert_ne!(replaced, identity);
        assert_eq!(parse(line.as_bytes(), 5, &replaced), None);
        // Nothing outside cache/ is written or read as a cache entry.
        fs.write_cache(Path::new("state/escape"), b"x");
        assert!(!root.join("state/escape").exists());
        assert_eq!(fs.read_cache(&blob), None);
        assert_eq!(entry_path("../../etc"), None);
    }
}
