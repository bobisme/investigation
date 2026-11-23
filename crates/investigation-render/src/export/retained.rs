//! Retained bytes an export may copy, checked against their digests. Pure.
//!
//! A kind that copies retained bytes (attachments in a review vault, blobs
//! in an archive, artifacts in a producer bundle) never reads the workspace
//! while it plans. The caller reads the bytes first and adds them here;
//! [`Retained::insert`] refuses bytes that do not hash to the digest they
//! were read for, so a planner can only copy bytes equal to the recorded
//! ones. A digest that was not added is simply absent: the planner decides
//! whether that is an omission or an error.

use std::collections::BTreeMap;

use investigation_core::wire::Digest;
use investigation_store::journal::hash_bytes;

/// Digest-checked retained bytes, by digest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Retained {
    blobs: BTreeMap<Digest, Vec<u8>>,
}

/// Bytes offered for a digest they do not hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestMismatch {
    pub expected: Digest,
    pub actual: Digest,
}

impl std::fmt::Display for DigestMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "The retained bytes read for {} hash to {}.",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for DigestMismatch {}

impl Retained {
    /// No retained bytes, for kinds that copy none.
    pub fn none() -> Self {
        Self::default()
    }

    /// Add `bytes` as the retained bytes of `expected`, after checking that
    /// they hash to it.
    pub fn insert(&mut self, expected: Digest, bytes: Vec<u8>) -> Result<(), DigestMismatch> {
        let actual = hash_bytes(&bytes);
        if actual != expected {
            return Err(DigestMismatch { expected, actual });
        }
        self.blobs.insert(expected, bytes);
        Ok(())
    }

    /// The bytes of `digest`, when they were added.
    pub fn get(&self, digest: &Digest) -> Option<&[u8]> {
        self.blobs.get(digest).map(Vec::as_slice)
    }

    /// Every digest held, in order.
    pub fn digests(&self) -> impl Iterator<Item = &Digest> {
        self.blobs.keys()
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}
