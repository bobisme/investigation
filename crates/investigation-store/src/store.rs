//! Coordinates canonical verification, validation and durable publication.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::{
    PROGRAM_NAME,
    catalog::{Catalog, standard_catalog},
    command::{Command, RevisionRef, SemanticRequest},
    evidence::{ArtifactBody, ObservationBasis},
    guardrails::{self, SessionEvidence},
    header::{Actor, Clock, SystemClock},
    identity::{Id, IdSource, Kind, SystemIdSource},
    state::{Acceptance, AcceptedTransaction, State, validate_and_apply_guarded},
    wire::{
        CommitResult, ErrorCode, Failure, Guidance, GuidanceKind, Projection as ReceiptProjection,
        ProjectionStatus, Receipt, ReceiptArtifact, ReceiptObservation, ResponseVersion, Snapshot,
    },
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    blob::{BlobError, StagedBlob, publish_blob, verify_existing},
    diag::{CommitTrace, Outcome, Stage},
    durable::{
        DurableError, DurableFile, DurableFs, LocalFs, LockError, WriteGate, publish,
        sync_directory, sync_file,
    },
    journal::{
        CaseGenesis, CaseReplay, Event, EventInput, VerifiedEvent, event_filename, hash_bytes,
        replay_case_from,
    },
    projection::{IndexEvent, Projection},
    workspace::Workspace,
};

#[path = "check.rs"]
mod check;
pub use check::{CheckIssue, CheckReport, Severity};

#[path = "ingest.rs"]
pub mod ingest;

#[path = "capture.rs"]
pub mod capture;

#[path = "evidence_read.rs"]
pub mod evidence_read;
use evidence_read::PromotionBlobs;

#[path = "method_packs.rs"]
mod method_packs;

#[path = "lessons.rs"]
mod lessons;

#[path = "search.rs"]
pub mod search;

#[path = "session_reads.rs"]
pub mod session_reads;

#[path = "exposure.rs"]
pub mod exposure;

#[path = "journal_heads.rs"]
mod journal_heads;

#[path = "archive.rs"]
pub mod archive;

#[path = "scope.rs"]
mod scope;
use scope::Reads;

#[path = "pins.rs"]
pub mod pins;

#[path = "retention.rs"]
pub mod retention;

#[path = "format_gate.rs"]
mod format_gate;
pub use format_gate::format_failure;

#[cfg(test)]
#[path = "closure_drill_tests.rs"]
mod closure_drill_tests;
#[cfg(test)]
#[path = "crash_tests.rs"]
mod crash_tests;
#[cfg(test)]
#[path = "diag_tests.rs"]
mod diag_tests;
#[cfg(test)]
#[path = "real_crash_tests.rs"]
mod real_crash_tests;
#[cfg(test)]
#[path = "retention_tests.rs"]
mod retention_tests;

const CASES: &str = "state/cases";
const TMP: &str = "state/tmp";
const BLOBS: &str = "state/blobs/sha256";
const COORDINATION_LOCK: &str = "state/locks/commit.lock";
const CASE_LOCK: &str = "commit.lock";
const RECOVERY_MARKER: &str = "local/writes-stopped";
const WAIT_FOR_LOCK: Duration = Duration::from_secs(2);

pub struct Store<F = LocalFs> {
    workspace: Workspace,
    fs: F,
    gate: WriteGate,
    catalog: Catalog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseHead {
    pub case_id: Id,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ReceiptLookup {
    Found { receipt: Box<Receipt> },
    NotFoundAtSnapshot { cases: Vec<CaseHead> },
}

impl Store {
    pub fn open(workspace: Workspace) -> Self {
        let fs = LocalFs::new(workspace.root());
        Self {
            workspace,
            fs,
            gate: WriteGate::default(),
            catalog: standard_catalog(),
        }
    }

    /// Make every later write a dry run: it takes the same lock, verifies the
    /// same journal and runs the same idempotency check, validation and gates
    /// as a commit, then returns the receipt the commit would return and
    /// publishes nothing. The receipt's identifiers and snapshot are those of
    /// a plan; a commit assigns its own. See [`crate::dry_run`].
    pub fn set_dry_run(&mut self) {
        self.fs.begin_dry_run();
    }

    /// Whether [`Self::set_dry_run`] was called.
    pub fn is_dry_run(&self) -> bool {
        self.fs.dry_run().is_some()
    }
}

impl<F: DurableFs> Store<F> {
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Parse and bound the request before acquiring the coordination lock.
    pub fn record(&mut self, input: &[u8]) -> Result<Receipt, Failure> {
        let (command, request) = Command::parse(input)?;
        self.commit(
            &command,
            request,
            &[],
            &mut SystemClock,
            &mut SystemIdSource,
        )
    }

    /// The head of every case, from a verified load of the whole workspace.
    pub fn list_cases(&mut self) -> Result<Vec<CaseHead>, Failure> {
        let _lock = self.coordinate()?;
        Ok(self.load_verified()?.heads())
    }

    /// The published case IDs, from the cases directory alone. Nothing is
    /// verified here: a command verifies the case it reads with
    /// [`Self::case`] (ADR 0011).
    pub fn case_ids(&mut self) -> Result<Vec<Id>, Failure> {
        let _lock = self.coordinate()?;
        self.published_cases()
    }

    /// Read a snapshot after the publisher has released the commit lock.
    /// Verifies this case alone, reusing its kept head (ADR 0010, 0011).
    pub fn case(&mut self, case_id: Id) -> Result<CaseReplay, Failure> {
        let _lock = self.coordinate()?;
        self.load_case_verified(case_id)?
            .ok_or_else(|| missing_case(case_id))
    }

    /// Read every case from one verified journal replay. The commit lock is
    /// held only while reading, so projections built from the result never
    /// block later commits.
    pub fn cases(&mut self) -> Result<BTreeMap<Id, CaseReplay>, Failure> {
        let _lock = self.coordinate()?;
        Ok(self.load_verified()?.cases)
    }

    pub fn receipt(&mut self, actor: &Actor, command_id: &str) -> Result<ReceiptLookup, Failure> {
        let _lock = self.coordinate()?;
        let workspace = self.load_verified()?;
        Ok(match workspace.find(actor, command_id) {
            Some(event) => {
                let mut receipt = receipt_for(event);
                let expected: Vec<_> = workspace
                    .heads()
                    .into_iter()
                    .map(|head| (head.case_id, head.snapshot))
                    .collect();
                receipt.projection.index =
                    if Projection::inspect(&self.index_path(), self.workspace.id()).as_ref()
                        == Ok(&expected)
                    {
                        ProjectionStatus::Current
                    } else {
                        ProjectionStatus::Degraded
                    };
                ReceiptLookup::Found {
                    receipt: Box::new(receipt),
                }
            }
            None => ReceiptLookup::NotFoundAtSnapshot {
                cases: workspace.heads(),
            },
        })
    }

    pub fn repair_index(&mut self) -> Result<Vec<CaseHead>, Failure> {
        let _lock = self.coordinate()?;
        let workspace = self.load_verified()?;
        self.rebuild_index(&workspace)?;
        Ok(workspace.heads())
    }

    fn commit(
        &mut self,
        command: &Command,
        request: SemanticRequest,
        blobs: &[StagedBlob],
        clock: &mut dyn Clock,
        ids: &mut dyn IdSource,
    ) -> Result<Receipt, Failure> {
        let observations = self.prepare_observations(command);
        let session_reads = command
            .session
            .as_ref()
            .map(|session| self.session_reads(&command.actor, session))
            .transpose()?;
        // Write-side guardrails see the same reads commit stamps, and those
        // stamped on the case's earlier events; only a guarded write needs them.
        let current_reads = guardrails::guards_any(command).then(|| session_reads.clone());
        let mut trace = CommitTrace::new(&command.command_id);
        let mut promotion = PromotionBlobs::default();
        // A promotion's retained evidence is known only from the locked
        // state. An attempt that needs a blob not yet hashed by this command
        // writes nothing and releases the lock; the blob is hashed and the
        // command validated again from a fresh replay.
        let result = loop {
            promotion.next_attempt();
            let attempt = self.commit_plan_traced(
                CommitPlan {
                    case_id: command.case_id,
                    reads: Reads::Target,
                    event: EventInput {
                        actor: &command.actor,
                        command_id: &command.command_id,
                        preconditions: &command.preconditions,
                        manifest_digest: None,
                        session_reads: session_reads.clone(),
                    },
                    request: request.clone(),
                    blobs,
                },
                clock,
                ids,
                |state, catalog, acceptance, visible| {
                    let sessions = current_reads.clone().map(|current| SessionEvidence {
                        current,
                        ledger: visible
                            .target
                            .map(CaseReplay::session_ledger)
                            .unwrap_or_default(),
                    });
                    validate_and_apply_guarded(
                        state,
                        command,
                        catalog,
                        acceptance,
                        &observations,
                        visible.packs,
                        sessions.as_ref(),
                    )
                    .map(Prepared::Accept)
                },
                &mut promotion,
                &mut trace,
            );
            match attempt {
                Ok(Some(receipt)) => break Ok(receipt),
                Ok(None) => {
                    trace.enter(Stage::VerifyArtifacts);
                    self.hash_pending(&mut promotion);
                }
                Err(failure) => break Err(failure),
            }
        };
        trace.finish(
            &command.command_id,
            command.case_id,
            match &result {
                Ok(_) => self.success_outcome(),
                Err(failure) => Outcome::Failed(failure),
            },
        );
        result.map_err(|failure| {
            if failure.error.code == ErrorCode::DurabilityUnknown {
                failure.with_argv(receipt_argv(&command.actor, &command.command_id))
            } else {
                failure
            }
        })
    }

    fn commit_plan(
        &mut self,
        plan: CommitPlan<'_>,
        clock: &mut dyn Clock,
        ids: &mut dyn IdSource,
        prepare: impl FnOnce(&State, &Catalog, Acceptance<'_>, Visible<'_>) -> Result<Prepared, Failure>,
    ) -> Result<Receipt, Failure> {
        let command_id = plan.event.command_id.to_owned();
        let case_id = plan.case_id;
        let mut trace = CommitTrace::new(&command_id);
        // These commands make no promotion of their own; any retained bytes
        // one needed would be hashed under the lock, as before.
        let result = self
            .commit_plan_traced(
                plan,
                clock,
                ids,
                prepare,
                &mut PromotionBlobs::under_lock(),
                &mut trace,
            )
            .and_then(|receipt| {
                receipt.ok_or_else(|| integrity("A commit was deferred without a retry."))
            });
        trace.finish(
            &command_id,
            case_id,
            match &result {
                Ok(_) => self.success_outcome(),
                Err(failure) => Outcome::Failed(failure),
            },
        );
        result
    }

    fn commit_plan_traced(
        &mut self,
        plan: CommitPlan<'_>,
        clock: &mut dyn Clock,
        ids: &mut dyn IdSource,
        prepare: impl FnOnce(&State, &Catalog, Acceptance<'_>, Visible<'_>) -> Result<Prepared, Failure>,
        promotion: &mut PromotionBlobs,
        trace: &mut CommitTrace,
    ) -> Result<Option<Receipt>, Failure> {
        let CommitPlan {
            case_id,
            reads,
            event: meta,
            request,
            blobs,
        } = plan;
        // Hash any blob these bytes deduplicate against before taking the
        // lock, so other writers never wait on work proportional to its size.
        for blob in blobs {
            verify_existing(&self.fs, Path::new(BLOBS), blob).map_err(blob_failure)?;
        }
        trace.enter(Stage::Lock);
        self.gate.check().map_err(durability_failure)?;
        let _coordination = self.coordinate()?;
        self.gate.check().map_err(durability_failure)?;
        // A newer format, or a migration in progress, stops every commit
        // (W11.15, ADR 0014); read from disk under the lock.
        self.refuse_unwritable_format()?;
        trace.enter(Stage::VerifyTail);
        let mut scope = self.load_scope(case_id, reads)?;
        self.fs.reclaim_publication_temporaries();
        trace.enter(Stage::Idempotency);
        if let Some((existing_case, offset)) =
            self.find_command(&mut scope, meta.actor, meta.command_id)?
        {
            let replay = &scope.replay;
            let existing = replay
                .cases
                .get(&existing_case)
                .and_then(|case| case.events.get(offset))
                .ok_or_else(|| integrity("A recorded command is missing."))?;
            if existing.event.request != request {
                return Err(Failure::new(
                    ErrorCode::IdempotencyKeyReused,
                    "This actor has committed a different request with the same command ID.",
                )
                .at("/command_id")
                .with_argv(receipt_argv(meta.actor, meta.command_id)));
            }
            let mut receipt = receipt_for(existing).as_retry();
            self.apply_projection_health(replay, &mut receipt);
            return Ok(Some(receipt));
        }

        trace.enter(Stage::Lock);
        let case = case_id
            .map(|id| {
                let case = scope
                    .replay
                    .cases
                    .remove(&id)
                    .ok_or_else(|| missing_case(id))?;
                Ok::<_, Failure>(FreeLater::new(case))
            })
            .transpose()?;
        let _case_lock = case_id
            .map(|id| {
                self.fs
                    .lock(&case_directory(id).join(CASE_LOCK), WAIT_FOR_LOCK)
                    .map_err(lock_failure)
            })
            .transpose()?;
        let initial = State::default();
        let original = case.as_ref().map_or(&initial, |case| &case.state);
        let sequence = original
            .sequence()
            .checked_add(1)
            .ok_or_else(|| integrity("Case sequence is exhausted."))?;
        trace.sequence(sequence);
        trace.enter(Stage::Validate);
        let prepared = prepare(
            original,
            &self.catalog,
            Acceptance {
                sequence,
                clock,
                ids,
            },
            Visible {
                target: case.as_deref(),
                others: &scope.replay,
                packs: &method_packs::RetainedPacks(&self.fs),
            },
        )
        .map_err(|error| add_conflict_context(error, original, case_id))?;
        let accepted = match prepared {
            Prepared::Accept(accepted) => accepted,
            Prepared::Existing {
                case_id: existing,
                sequence,
            } => {
                if case_id != Some(existing) {
                    self.ensure_loaded(&mut scope, existing)?;
                }
                let replay = &mut scope.replay;
                let events = match &case {
                    Some(loaded) if case_id == Some(existing) => &loaded.events,
                    _ => {
                        &replay
                            .cases
                            .get(&existing)
                            .ok_or_else(|| missing_case(existing))?
                            .events
                    }
                };
                let event = usize::try_from(sequence.saturating_sub(1))
                    .ok()
                    .and_then(|offset| events.get(offset))
                    .ok_or_else(|| integrity("An existing transaction is missing."))?;
                let mut receipt = receipt_for(event).as_retry();
                // Restore the target case so projection health sees every head.
                if let (Some(loaded), Some(id)) = (case, case_id) {
                    replay.cases.insert(id, loaded.into_inner());
                }
                self.apply_projection_health(replay, &mut receipt);
                return Ok(Some(receipt));
            }
        };

        // Exercise the same replay checks before touching canonical bytes.
        let mut projected = original.clone();
        projected.apply(&accepted)?;
        self.verify_promotion_artifacts(&projected, &accepted, promotion)?;
        self.verify_method_pack_adoptions(&accepted)?;
        // A consultation reads its lesson's case: verify that case first.
        for source in lessons::source_cases(&accepted) {
            if source != accepted.case_id {
                self.ensure_loaded(&mut scope, source)?;
            }
        }
        lessons::verify_consultations(&scope.replay.cases, &accepted)?;
        if !promotion.pending.is_empty() {
            // Nothing is written yet: the caller hashes these blobs after
            // the lock is released and retries.
            return Ok(None);
        }
        let case_id = accepted.case_id;
        let transaction_id = ids
            .next_id(Kind::Transaction)
            .map_err(|_| integrity("Could not allocate a transaction ID."))?;
        trace.accepted(&accepted, transaction_id);
        let allocated = accepted
            .created
            .iter()
            .map(|change| change.id)
            .chain([transaction_id]);
        if self.already_recorded(&scope, allocated)? {
            return Err(integrity("ID generator returned an already recorded ID."));
        }
        let genesis = case
            .is_none()
            .then(|| CaseGenesis::new(self.workspace.id(), case_id)?.to_bytes())
            .transpose()?;
        let previous_hash = match (&case, &genesis) {
            (Some(loaded), _) => loaded.snapshot.head_hash.clone(),
            (_, Some(bytes)) => hash_bytes(bytes),
            _ => return Err(integrity("Expected a case head or a new genesis.")),
        };
        let event = Event::new(
            self.workspace.id(),
            transaction_id,
            previous_hash,
            meta,
            request,
            accepted,
        )?;
        let bytes = event.to_bytes()?;
        let hash = hash_bytes(&bytes);
        let name = event_filename(sequence)?;
        if let Some(dry) = self.fs.dry_run_mut() {
            // Everything a commit checks has passed; publish nothing.
            dry.plan(case_id, projected);
            let mut receipt = receipt_for(&VerifiedEvent { event, hash });
            receipt.projection.index = ProjectionStatus::Pending;
            return Ok(Some(receipt));
        }
        self.persist(&event, &bytes, &name, genesis.as_deref(), blobs, trace)
            .inspect_err(|error| {
                if error.error.code == ErrorCode::DurabilityUnknown {
                    self.note_stopped();
                }
            })?;
        // The heads this commit verified, before its own event; the next
        // load verifies that event in full.
        let replay = &mut scope.replay;
        self.keep_verified_heads(replay);

        let verified = VerifiedEvent { event, hash };
        let mut receipt = receipt_for(&verified);
        let snapshot = receipt.snapshot.clone();
        let mut events = case.map_or_else(Vec::new, |case| case.into_inner().events);
        events.push(verified);
        replay.cases.insert(
            case_id,
            CaseReplay {
                state: projected,
                snapshot,
                events,
            },
        );

        // Past durable publication, projection failures cannot uncommit.
        trace.enter(Stage::IndexUpdate);
        self.apply_projection_health(replay, &mut receipt);
        trace.enter(Stage::Receipt);
        Ok(Some(receipt))
    }

    fn persist(
        &mut self,
        event: &Event,
        bytes: &[u8],
        name: &str,
        genesis: Option<&[u8]>,
        blobs: &[StagedBlob],
        trace: &mut CommitTrace,
    ) -> Result<(), Failure> {
        trace.enter(Stage::BlobPublish);
        let expected = event
            .request
            .attachments
            .iter()
            .chain(event.manifest_digest.iter());
        if event.request.attachments.len() + usize::from(event.manifest_digest.is_some())
            != blobs.len()
            || expected.zip(blobs).any(|(digest, blob)| {
                digest.as_str().strip_prefix("sha256:") != Some(blob.digest.as_str())
            })
        {
            return Err(integrity(
                "Staged blobs disagree with the attachment digests.",
            ));
        }
        for blob in blobs {
            let (target, linked) = publish_blob(&mut self.fs, &self.gate, Path::new(BLOBS), blob)
                .map_err(blob_failure)?;
            // The linked file holds the bytes staging hashed as it wrote them.
            if linked && let Ok(identity) = self.fs.identity(&target) {
                self.record_verdict(&blob.digest, blob.size, &identity, true);
            }
        }

        let directory = case_directory(event.accepted.case_id);
        let Some(genesis) = genesis else {
            let staged = Path::new(TMP).join(format!("event-{}.tmp", event.transaction_id));
            trace.enter(Stage::JournalWrite);
            write_private(&mut self.fs, &self.gate, &staged, bytes, true)?;
            trace.enter(Stage::JournalPublish);
            let result = publish(
                &mut self.fs,
                &staged,
                &directory.join("events").join(name),
                &self.gate,
            );
            if result.is_ok() {
                let _ = self.fs.remove(&staged);
            }
            return result.map_err(exclusive_publication_failure);
        };

        // Publish the directory containing both genesis and the first event.
        let staged = Path::new(TMP).join(format!("create-{}", event.transaction_id));
        trace.enter(Stage::JournalWrite);
        let events = staged.join("events");
        self.fs.create_dir(&staged).map_err(io_failure)?;
        self.fs.create_dir(&events).map_err(io_failure)?;
        write_private(
            &mut self.fs,
            &self.gate,
            &staged.join("genesis.json"),
            genesis,
            true,
        )?;
        write_private(
            &mut self.fs,
            &self.gate,
            &staged.join(CASE_LOCK),
            b"",
            false,
        )?;
        write_private(&mut self.fs, &self.gate, &events.join(name), bytes, true)?;
        trace.enter(Stage::DirectorySync);
        sync_directory(&mut self.fs, &events, &self.gate).map_err(durability_failure)?;
        sync_directory(&mut self.fs, &staged, &self.gate).map_err(durability_failure)?;
        trace.enter(Stage::JournalPublish);
        self.fs
            .rename_no_replace(&staged, &directory)
            .map_err(|error| exclusive_publication_failure(DurableError::Io(error)))?;
        trace.enter(Stage::DirectorySync);
        sync_directory(&mut self.fs, Path::new(CASES), &self.gate).map_err(durability_failure)
    }

    fn success_outcome(&self) -> Outcome<'static> {
        if self.fs.dry_run().is_some() {
            Outcome::DryRun
        } else {
            Outcome::Committed
        }
    }

    /// The state the last write left `case_id` in: after a dry run that
    /// planned a transaction in it, the state that transaction would leave.
    pub(crate) fn state_after_write(&mut self, case_id: Id) -> Result<State, Failure> {
        if let Some(planned) = self.fs.dry_run().and_then(|dry| dry.planned(case_id)) {
            return Ok(planned.clone());
        }
        Ok(self.case(case_id)?.state)
    }

    fn coordinate(&mut self) -> Result<F::Guard, Failure> {
        self.gate.check().map_err(durability_failure)?;
        self.fs
            .lock(Path::new(COORDINATION_LOCK), WAIT_FOR_LOCK)
            .map_err(lock_failure)
    }

    /// Verify the whole journal, reusing the case heads verified earlier
    /// (see `journal_heads`). Writes nothing: only a commit that published
    /// keeps the heads this load verified, with [`Self::keep_verified_heads`].
    fn load_verified(&self) -> Result<WorkspaceReplay, Failure> {
        self.load(true)
    }

    /// Verify the whole journal from its bytes alone, ignoring any kept head.
    fn load_fully_verified(&self) -> Result<WorkspaceReplay, Failure> {
        self.load(false)
    }

    fn load(&self, reuse_heads: bool) -> Result<WorkspaceReplay, Failure> {
        let mut result = WorkspaceReplay {
            complete: true,
            ..WorkspaceReplay::default()
        };
        for case_id in self.published_cases()? {
            if !self.load_case_into(&mut result, case_id, reuse_heads)? {
                return Err(integrity("A published case disappeared during a load."));
            }
        }
        Ok(result)
    }

    /// Keep the heads a load verified that differ from the kept ones. A
    /// commit calls this only once it published, so a refused or retried
    /// command writes nothing, and a read never does.
    fn keep_verified_heads(&self, replay: &WorkspaceReplay) {
        for (case_id, snapshot) in &replay.unkept {
            self.keep_head(*case_id, snapshot);
        }
    }

    fn index_path(&self) -> PathBuf {
        self.workspace.root().join("cache/index.sqlite")
    }

    fn rebuild_index(&self, workspace: &WorkspaceReplay) -> Result<(), Failure> {
        if !workspace.complete {
            // A rebuild needs every case, each verified.
            return self.rebuild_index(&FreeLater::new(self.load_verified()?));
        }
        let receipts = workspace.receipts();
        Projection::rebuild(
            &self.index_path(),
            self.workspace.id(),
            workspace.frames(&receipts),
        )?;
        Ok(())
    }

    fn refresh_index(&self, workspace: &WorkspaceReplay) -> Result<(), Failure> {
        let mut index = match Projection::open(&self.index_path(), self.workspace.id()) {
            Ok(index) => index,
            Err(_) => return self.rebuild_index(workspace),
        };
        let heads: BTreeMap<_, _> = match index.heads() {
            Ok(heads) => heads.into_iter().collect(),
            Err(_) => {
                drop(index);
                return self.rebuild_index(workspace);
            }
        };
        // A case-scoped load (ADR 0011) holds the cases whose indexed head
        // was not their canonical head when it loaded, under this same lock;
        // the index is current for every other case.
        let valid_prefix = heads.iter().all(|(id, head)| {
            if !workspace.complete && !workspace.cases.contains_key(id) {
                return true;
            }
            head.sequence
                .checked_sub(1)
                .and_then(|offset| usize::try_from(offset).ok())
                .and_then(|offset| workspace.cases.get(id)?.events.get(offset))
                .is_some_and(|verified| verified.hash == head.head_hash)
        });
        if !valid_prefix {
            drop(index);
            return self.rebuild_index(workspace);
        }
        // Build receipts only for the events the index has not applied.
        for (case_id, case) in &workspace.cases {
            let applied = heads.get(case_id).map_or(0, |head| head.sequence);
            let skip = usize::try_from(applied).unwrap_or(usize::MAX);
            for verified in case.events.iter().skip(skip) {
                let event = &verified.event;
                index.apply(IndexEvent {
                    previous_hash: &event.previous_hash,
                    actor: &event.actor,
                    accepted: &event.accepted,
                    receipt: &receipt_for(verified),
                    relations: &[],
                })?;
            }
        }
        Ok(())
    }

    fn apply_projection_health(&self, replay: &WorkspaceReplay, receipt: &mut Receipt) {
        if self.fs.dry_run().is_some() {
            // A dry run leaves the index as it is.
            receipt.projection.index = ProjectionStatus::Pending;
        } else if self.refresh_index(replay).is_err() {
            receipt.projection.index = ProjectionStatus::Degraded;
            receipt.warnings.push("The canonical transaction is committed. Run investigation repair index to rebuild the disposable index.".to_owned());
        } else {
            receipt.projection.index = ProjectionStatus::Current;
        }
    }

    fn note_stopped(&mut self) {
        // Best-effort diagnostic. Do not retry the failed filesystem sync.
        if let Ok(mut marker) = self.fs.create_private(Path::new(RECOVERY_MARKER)) {
            let _ = marker.write_all(b"A writer stopped after an uncertain sync. Verify the journal and retry with the same command ID.\n");
        }
    }
}

/// A large verified replay, freed on another thread when it goes out of
/// scope, so neither the caller nor the commit lock waits on deallocating
/// every record of the journal. Freed in place if no thread can start.
struct FreeLater<T: Send + 'static>(Option<T>);

impl<T: Send + 'static> FreeLater<T> {
    fn new(value: T) -> Self {
        Self(Some(value))
    }

    fn into_inner(mut self) -> T {
        self.0.take().expect("present until dropped")
    }
}

impl<T: Send + 'static> std::ops::Deref for FreeLater<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_ref().expect("present until dropped")
    }
}

impl<T: Send + 'static> std::ops::DerefMut for FreeLater<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0.as_mut().expect("present until dropped")
    }
}

/// Free a large value, such as a gap inventory of hundreds of thousands of
/// gaps, on another thread, so an answer does not wait on deallocating it.
pub fn free_later<T: Send + 'static>(value: T) {
    drop(FreeLater::new(value));
}

impl<T: Send + 'static> Drop for FreeLater<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            // On failure the closure, and the value with it, is dropped here.
            let _ = std::thread::Builder::new().spawn(move || drop(value));
        }
    }
}

/// Verified canonical cases a preparation may read under the commit lock.
struct Visible<'a> {
    /// The target case, when it already exists.
    target: Option<&'a CaseReplay>,
    /// The other cases: every one when the plan reads [`Reads::Workspace`],
    /// otherwise only those loaded so far. See [`WorkspaceReplay::all`].
    others: &'a WorkspaceReplay,
    /// Retained method pack bytes, read under the commit lock for policy gates.
    packs: &'a dyn investigation_core::pack_policy::PackSource,
}

/// What a preparation decided under the commit lock.
enum Prepared {
    Accept(AcceptedTransaction),
    /// Nothing new to record: answer with an earlier transaction's receipt.
    Existing {
        case_id: Id,
        sequence: u64,
    },
}

struct CommitPlan<'a> {
    case_id: Option<Id>,
    /// What the preparation reads beyond the target case.
    reads: Reads,
    event: EventInput<'a>,
    request: SemanticRequest,
    blobs: &'a [StagedBlob],
}

#[derive(Default)]
struct WorkspaceReplay {
    /// Verified cases: every published case when `complete`, otherwise the
    /// cases a case-scoped command loaded (ADR 0011).
    cases: BTreeMap<Id, CaseReplay>,
    complete: bool,
    commands: BTreeMap<(Actor, String), (Id, usize)>,
    transactions: BTreeSet<Id>,
    records: BTreeSet<Id>,
    /// Verified case heads that differ from the kept ones (`journal_heads`).
    unkept: Vec<(Id, Snapshot)>,
}

impl WorkspaceReplay {
    /// Every verified case, for a preparation that searches the workspace.
    /// A case-scoped load has not verified every case, so it refuses.
    fn all(&self) -> Result<&BTreeMap<Id, CaseReplay>, Failure> {
        if self.complete {
            Ok(&self.cases)
        } else {
            Err(integrity(
                "A preparation searched other cases without loading the workspace.",
            ))
        }
    }

    fn heads(&self) -> Vec<CaseHead> {
        self.cases
            .iter()
            .map(|(&case_id, replay)| CaseHead {
                case_id,
                snapshot: replay.snapshot.clone(),
            })
            .collect()
    }

    fn find(&self, actor: &Actor, command: &str) -> Option<&VerifiedEvent> {
        let (case, index) = self.commands.get(&(actor.clone(), command.to_owned()))?;
        self.cases.get(case)?.events.get(*index)
    }

    fn receipts(&self) -> Vec<Receipt> {
        self.cases
            .values()
            .flat_map(|case| case.events.iter().map(receipt_for))
            .collect()
    }

    fn frames<'a>(&'a self, receipts: &'a [Receipt]) -> impl Iterator<Item = IndexEvent<'a>> + 'a {
        self.cases
            .values()
            .flat_map(|case| case.events.iter())
            .zip(receipts)
            .map(|(verified, receipt)| {
                let event = &verified.event;
                IndexEvent {
                    previous_hash: &event.previous_hash,
                    actor: &event.actor,
                    accepted: &event.accepted,
                    receipt,
                    relations: &[],
                }
            })
    }
}

fn receipt_for(verified: &VerifiedEvent) -> Receipt {
    let event = &verified.event;
    let artifacts = event
        .accepted
        .records
        .iter()
        .filter(|record| record.header.id.kind() == Kind::Artifact)
        .filter_map(|record| {
            let artifact: ArtifactBody = investigation_core::input::decode(&record.body).ok()?;
            // Only retained bytes are listed: a reference-only digest is a claim.
            if artifact.availability != investigation_core::evidence::ArtifactAvailability::Retained
            {
                return None;
            }
            Some(ReceiptArtifact {
                artifact: record.reference(),
                digest: artifact.digest?,
                size_bytes: artifact.size_bytes?,
            })
        })
        .collect();
    let observation_checks = event
        .accepted
        .records
        .iter()
        .filter(|record| record.header.id.kind() == Kind::Observation)
        .filter_map(|record| {
            let basis: ObservationBasis =
                investigation_core::input::decode(&record.body["basis"]).ok()?;
            let ObservationBasis::Source {
                selector,
                text_quote,
                ..
            } = basis
            else {
                return None;
            };
            Some(ReceiptObservation {
                observation: record.reference(),
                selector_validation: selector.validation,
                quote_state: text_quote.state,
            })
        })
        .collect();
    Receipt {
        schema_version: ResponseVersion::V1,
        ok: true,
        result: CommitResult::Committed,
        command_id: event.command_id.clone(),
        transaction_id: event.transaction_id,
        snapshot: Snapshot {
            sequence: event.sequence,
            head_hash: verified.hash.clone(),
        },
        created: event.accepted.created.clone(),
        updated: event.accepted.updated.clone(),
        artifacts,
        observation_checks,
        projection: ReceiptProjection {
            index: ProjectionStatus::Pending,
            vault: ProjectionStatus::Disabled,
        },
        warnings: event.accepted.warnings.clone(),
        next: event
            .accepted
            .created
            .first()
            .map(|record| Guidance {
                kind: GuidanceKind::LocalRead,
                argv: show_argv(record.id, event.accepted.case_id),
            })
            .into_iter()
            .collect(),
    }
}

fn write_private(
    fs: &mut impl DurableFs,
    gate: &WriteGate,
    path: &Path,
    bytes: &[u8],
    immutable: bool,
) -> Result<(), Failure> {
    gate.check().map_err(durability_failure)?;
    let mut file = fs.create_private(path).map_err(io_failure)?;
    file.write_all(bytes).map_err(io_failure)?;
    if immutable {
        file.set_readonly().map_err(io_failure)?;
    }
    sync_file(&mut file, gate).map_err(durability_failure)
}

fn case_directory(case_id: Id) -> PathBuf {
    Path::new(CASES).join(case_id.to_string())
}

fn add_conflict_context(mut failure: Failure, state: &State, case_id: Option<Id>) -> Failure {
    if failure.error.code != ErrorCode::RevisionConflict {
        return failure;
    }
    let parse = |value: Option<&Value>| serde_json::from_value::<RevisionRef>(value?.clone()).ok();
    let Some(actual) = parse(failure.error.actual.as_ref()) else {
        return failure;
    };
    let Some(case_id) = case_id else {
        return failure;
    };
    let changed: Vec<_> = parse(failure.error.expected.as_ref())
        .and_then(|reference| state.revision(reference))
        .and_then(|old| {
            Some((
                old.body.as_object()?,
                state.current(actual.id)?.body.as_object()?,
            ))
        })
        .map_or_else(Vec::new, |(old, current)| {
            let keys: BTreeSet<_> = old.keys().chain(current.keys()).collect();
            keys.into_iter()
                .filter(|key| old.get(*key) != current.get(*key))
                .take(12)
                .map(|key| key.chars().take(80).collect::<String>())
                .collect()
        });
    failure.error.retry = Some("reassess".into());
    failure.error.actual =
        Some(json!({"id":actual.id, "revision":actual.revision, "changed_fields_sample":changed}));
    failure.with_argv(show_argv(actual.id, case_id))
}

fn show_argv(id: Id, case_id: Id) -> Vec<String> {
    vec![
        PROGRAM_NAME.into(),
        "show".into(),
        id.to_string(),
        "--case".into(),
        case_id.to_string(),
        "--format".into(),
        "json".into(),
    ]
}

fn receipt_argv(actor: &Actor, command_id: &str) -> Vec<String> {
    vec![
        PROGRAM_NAME.into(),
        "receipt".into(),
        command_id.into(),
        "--actor".into(),
        actor.to_string(),
        "--format".into(),
        "json".into(),
    ]
}

fn read_argv(command: &str) -> Vec<String> {
    [PROGRAM_NAME, command, "--format", "json"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn missing_case(id: Id) -> Failure {
    Failure::new(ErrorCode::CaseRequired, format!("Case {id} is absent."))
        .at("/case_id")
        .with_argv(read_argv("onboard"))
}

fn integrity(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::IntegrityFailure, message).with_argv(read_argv("check"))
}

fn io_failure(error: io::Error) -> Failure {
    Failure::new(
        ErrorCode::IoFailure,
        format!(
            "Workspace filesystem operation failed ({:?}).",
            error.kind()
        ),
    )
    .with_argv(read_argv("check"))
}

fn lock_failure(error: LockError) -> Failure {
    match error {
        LockError::Busy { waited } => Failure::new(
            ErrorCode::WorkspaceBusy,
            format!(
                "Timed out after {} ms waiting for the workspace commit lock. Retry the identical request.",
                waited.as_millis()
            ),
        ),
        LockError::Io(error) => io_failure(error),
    }
}

fn durability_failure(error: DurableError) -> Failure {
    match error {
        DurableError::Io(error) => io_failure(error),
        DurableError::SyncUnknown(_) | DurableError::Stopped => Failure::new(
            ErrorCode::DurabilityUnknown,
            "A filesystem sync failed earlier in this process. Verify the journal in a fresh process and retry with the same actor and command ID.",
        ).with_argv(read_argv("check")),
    }
}

fn exclusive_publication_failure(error: DurableError) -> Failure {
    match error {
        DurableError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists => integrity(
            "The canonical publication destination already exists. Inspect the journal for a fork.",
        ),
        other => durability_failure(other),
    }
}

fn blob_failure(error: BlobError) -> Failure {
    match error {
        BlobError::Io(error) => io_failure(error),
        BlobError::Durable(error) => durability_failure(error),
        BlobError::DigestMismatch => integrity("A stored blob does not match its recorded digest."),
    }
}
