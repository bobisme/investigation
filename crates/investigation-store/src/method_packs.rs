//! Method pack adoption checks and read-time resolution from retained bytes.
//!
//! Adoption is verified under the commit lock against the retained blob of the
//! pinned digest, and resolution reads the same blob. Neither consults the
//! path the pack was ingested from, so editing that file cannot change a case.
use std::io::Read;

use investigation_core::{
    input,
    method_pack::{self, MAX_PACK_BYTES},
    method_pack_ops::{self, MethodPackPin, ResolvedPack},
};

use super::*;

impl<F: DurableFs> Store<F> {
    /// The packs that apply to a case: the built-in pack plus every pin on
    /// the case at its head, or as of an earlier acceptance sequence. A pin
    /// whose retained bytes are missing or unusable resolves as unavailable;
    /// it is never an error for the read.
    pub fn method_packs(
        &self,
        state: &State,
        case_id: Id,
        at_sequence: Option<u64>,
    ) -> Result<Vec<ResolvedPack>, Failure> {
        let pins = method_pack_ops::adopted_packs(state, case_id, at_sequence)?;
        Ok(method_pack_ops::resolve(&pins, |pin| {
            self.retained_pack_bytes(pin)
        }))
    }

    fn retained_pack_bytes(&self, pin: &MethodPackPin) -> Option<Vec<u8>> {
        let hex = pin.digest.as_str().strip_prefix("sha256:")?;
        let path = Path::new(BLOBS).join(&hex[..2]).join(hex);
        let mut bytes = Vec::new();
        self.fs
            .read(&path)
            .ok()?
            .take(MAX_PACK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Reject a new pin unless its retained bytes load as the declared pack.
    pub(super) fn verify_method_pack_adoptions(
        &self,
        accepted: &AcceptedTransaction,
    ) -> Result<(), Failure> {
        for record in &accepted.records {
            if record.header.id.kind() != Kind::Case {
                continue;
            }
            let Some(value) = record.body.get("adopted_method_packs") else {
                continue;
            };
            let pins: Vec<MethodPackPin> = input::decode(value)
                .map_err(|_| integrity("Accepted method pack pins have an invalid shape."))?;
            for pin in pins
                .iter()
                .filter(|pin| pin.adopted_at_sequence == accepted.sequence)
            {
                self.verify_pin(pin)?;
            }
        }
        Ok(())
    }

    fn verify_pin(&self, pin: &MethodPackPin) -> Result<(), Failure> {
        let bytes = self.retained_pack_bytes(pin).ok_or_else(|| {
            integrity("The retained bytes of the cited pack Artifact are missing or unreadable.")
        })?;
        let rejected = |message: &str, actual: Value| {
            let mut failure = Failure::new(ErrorCode::InvalidInput, message)
                .with_schema(investigation_core::method_pack_ops::ADOPT);
            failure.error.actual = Some(actual);
            failure
        };
        let loaded = method_pack::load(&bytes).map_err(|error| {
            rejected(
                "The cited Artifact is not a loadable method pack.",
                json!({"pack_error": error.message, "pack_path": error.path, "line": error.line}),
            )
        })?;
        if loaded.digest() != &pin.digest
            || loaded.id() != pin.pack_id
            || loaded.version() != pin.version
        {
            return Err(rejected(
                "The pack file declares another id, version or digest than the adoption.",
                json!({"pack_id": loaded.id(), "version": loaded.version(), "digest": loaded.digest()}),
            ));
        }
        Ok(())
    }
}
