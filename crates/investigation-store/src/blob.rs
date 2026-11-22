//! Streaming immutable blob staging. The caller coordinates orphan cleanup and journal commit.
use crate::durable::{
    DurableError, DurableFile, DurableFs, WriteGate, publish, sync_directory, sync_file,
};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum BlobError {
    Durable(DurableError),
    Io(io::Error),
    DigestMismatch,
}
impl From<DurableError> for BlobError {
    fn from(e: DurableError) -> Self {
        Self::Durable(e)
    }
}
impl From<io::Error> for BlobError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBlob {
    pub temp: PathBuf,
    pub digest: String,
    pub size: u64,
}
/// `temp` must be a unique private path under state/tmp on the canonical filesystem.
pub fn stage(
    fs: &mut impl DurableFs,
    gate: &WriteGate,
    temp: &Path,
    mut input: impl Read,
) -> Result<StagedBlob, BlobError> {
    gate.check()?;
    let mut file = fs.create_private(temp)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("blob too large"))?;
    }
    file.set_readonly()?;
    sync_file(&mut file, gate)?;
    drop(file);
    Ok(StagedBlob {
        temp: temp.to_owned(),
        digest: format!("{:x}", hash.finalize()),
        size,
    })
}
/// `base` is the existing durable `state/blobs/sha256` directory. A new shard is synced before use.
pub fn publish_blob(
    fs: &mut impl DurableFs,
    gate: &WriteGate,
    base: &Path,
    staged: &StagedBlob,
) -> Result<PathBuf, BlobError> {
    gate.check()?;
    if staged.digest.len() != 64
        || !staged
            .digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid SHA-256 digest").into());
    }
    let shard = base.join(&staged.digest[..2]);
    match fs.create_dir(&shard) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    sync_directory(fs, base, gate)?;
    let target = shard.join(&staged.digest);
    match publish(fs, &staged.temp, &target, gate) {
        Ok(()) => {}
        Err(DurableError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
            verify(fs.read(&target)?, &staged.digest, staged.size)?;
            sync_directory(fs, &shard, gate)?;
        }
        Err(e) => return Err(e.into()),
    }
    // An orphan temp is safe if cleanup fails; it is never a writable producer link.
    let _ = fs.remove(&staged.temp);
    Ok(target)
}
fn verify(mut input: impl Read, digest: &str, size: u64) -> Result<(), BlobError> {
    let mut hash = Sha256::new();
    let mut nbytes = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        nbytes = nbytes
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("blob too large"))?;
    }
    if nbytes != size || format!("{:x}", hash.finalize()) != digest {
        return Err(BlobError::DigestMismatch);
    }
    Ok(())
}
/// Verify a blob with bounded memory before its bytes are trusted by a caller.
pub fn verify_blob(
    fs: &impl DurableFs,
    path: &Path,
    digest: &str,
    size: u64,
) -> Result<(), BlobError> {
    verify(fs.read(path)?, digest, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable::MemoryFs;
    #[test]
    fn deduplicates_and_detects_wrong_content() {
        let mut fs = MemoryFs::new();
        for dir in ["state", "state/tmp", "state/blobs", "state/blobs/sha256"] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
        let gate = WriteGate::isolated();
        let base = Path::new("state/blobs/sha256");
        let first = stage(&mut fs, &gate, Path::new("state/tmp/a"), &b"hello"[..]).unwrap();
        let target = publish_blob(&mut fs, &gate, base, &first).unwrap();
        let second = stage(&mut fs, &gate, Path::new("state/tmp/b"), &b"hello"[..]).unwrap();
        assert_eq!(publish_blob(&mut fs, &gate, base, &second).unwrap(), target);
        verify_blob(&fs, &target, &first.digest, first.size).unwrap();
        assert!(matches!(
            verify_blob(&fs, &target, &"0".repeat(64), first.size),
            Err(BlobError::DigestMismatch)
        ));
    }
}
