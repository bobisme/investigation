//! Ordered command validation and replay of accepted record revisions.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    basis::MaterialAssumptions,
    catalog::Catalog,
    command::{Command, Reference, RevisionRef},
    guardrails::SessionEvidence,
    header::{Actor, Classification, Clock, RecordHeader, Revision, SchemaVersion, UtcTimestamp},
    identity::{Id, IdSource, Kind},
    observation_ops::ObservationProof,
    pack_policy::PackSource,
    wire::{Digest, ErrorCode, Failure, RecordChange, Snapshot},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub header: RecordHeader,
    pub body: Value,
}

impl Record {
    pub fn reference(&self) -> RevisionRef {
        RevisionRef {
            id: self.header.id,
            revision: self.header.revision.0,
        }
    }
}

/// Canonical after-images in application order, plus the changes issued to the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedTransaction {
    pub case_id: Id,
    pub sequence: u64,
    pub records: Vec<Record>,
    pub created: Vec<RecordChange>,
    pub updated: Vec<RecordChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    sequence: u64,
    history: BTreeMap<Id, Vec<Record>>,
    verified_snapshots: BTreeMap<u64, Digest>,
    derived: Derived,
}

/// Indexes computed from the history on first use and dropped whenever the
/// history changes, so a read that asks the same question of many records
/// pays for one pass over the snapshot. They are a function of the history
/// alone: never part of equality, and a clone may keep them.
#[derive(Clone, Default)]
struct Derived {
    material_assumptions: OnceLock<BTreeMap<Id, MaterialAssumptions>>,
}

impl PartialEq for Derived {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Derived {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Derived")
    }
}

impl State {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Associate the current replay sequence with a hash verified by its journal reader.
    /// Accepted after-images alone cannot authenticate a snapshot.
    pub fn note_verified_snapshot(&mut self, snapshot: Snapshot) -> Result<(), Failure> {
        if snapshot.sequence != self.sequence
            || self
                .verified_snapshot(snapshot.sequence)
                .is_some_and(|known| known != &snapshot.head_hash)
        {
            return Err(Failure::new(
                ErrorCode::IntegrityFailure,
                "Verified snapshot does not match the replayed case sequence or hash.",
            ));
        }
        self.verified_snapshots
            .insert(snapshot.sequence, snapshot.head_hash);
        Ok(())
    }

    pub fn verified_snapshot(&self, sequence: u64) -> Option<&Digest> {
        self.verified_snapshots.get(&sequence)
    }

    pub fn record_at(&self, id: Id, sequence: u64) -> Option<&Record> {
        let records = self.history.get(&id)?;
        let end = records.partition_point(|record| record.header.acceptance_sequence <= sequence);
        records.get(end.checked_sub(1)?)
    }

    /// The state as replayed up to an earlier accepted sequence: every record
    /// revision accepted at or before it, and the snapshots verified up to it.
    pub fn as_of(&self, sequence: u64) -> State {
        let history = self
            .history
            .iter()
            .filter_map(|(id, records)| {
                let end =
                    records.partition_point(|record| record.header.acceptance_sequence <= sequence);
                (end > 0).then(|| (*id, records[..end].to_vec()))
            })
            .collect();
        State {
            sequence: sequence.min(self.sequence),
            history,
            verified_snapshots: self
                .verified_snapshots
                .range(..=sequence)
                .map(|(sequence, digest)| (*sequence, digest.clone()))
                .collect(),
            derived: Derived::default(),
        }
    }

    pub fn history(&self, id: Id) -> impl Iterator<Item = &Record> {
        self.history.get(&id).into_iter().flatten()
    }

    pub fn current(&self, id: Id) -> Option<&Record> {
        self.history.get(&id)?.last()
    }

    pub fn revision(&self, reference: RevisionRef) -> Option<&Record> {
        let index = usize::try_from(reference.revision.checked_sub(1)?).ok()?;
        self.history.get(&reference.id)?.get(index)
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.history.values().filter_map(|records| records.last())
    }

    /// Current records of one kind, in ID order: the records of that kind
    /// [`Self::records`] yields, without visiting the others.
    pub fn records_of_kind(&self, kind: Kind) -> impl Iterator<Item = &Record> {
        self.history
            .range(Id::kind_range(kind))
            .filter_map(|(_, records)| records.last())
    }

    /// Material-assumption membership of every subject, per case, at this
    /// snapshot.
    pub(crate) fn material_assumptions(&self) -> &BTreeMap<Id, MaterialAssumptions> {
        self.derived
            .material_assumptions
            .get_or_init(|| MaterialAssumptions::index(self))
    }

    /// Apply already accepted images atomically, without a catalog, clock or ID source.
    /// Journal sequence continuity and hash verification belong to the journal reader.
    pub fn apply(&mut self, transaction: &AcceptedTransaction) -> Result<(), Failure> {
        let integrity = |message| Failure::new(ErrorCode::IntegrityFailure, message);
        if transaction.sequence <= self.sequence
            || transaction.case_id.kind() != Kind::Case
            || transaction.records.is_empty()
        {
            return Err(integrity(
                "Invalid accepted transaction identity or sequence.",
            ));
        }
        let mut created = transaction.created.iter();
        let mut updated = transaction.updated.iter();
        let mut aliases = BTreeSet::new();
        let mut pending = BTreeMap::new();
        for record in &transaction.records {
            let header = &record.header;
            if header.case_id != transaction.case_id
                || header.acceptance_sequence != transaction.sequence
                || !record.body.is_object()
            {
                return Err(integrity(
                    "Accepted record has invalid case, sequence or body.",
                ));
            }
            let current = pending
                .get(&header.id)
                .copied()
                .or_else(|| self.current(header.id));
            let change = if let Some(current) = current {
                let old = &current.header;
                if old.case_id != header.case_id
                    || old.schema_version != header.schema_version
                    || old.created_at != header.created_at
                    || header.history.split_last() != Some((&old.revision, old.history.as_slice()))
                    || old.created_by != header.created_by
                    || old.actor_descriptor_self_reported != header.actor_descriptor_self_reported
                    || old.revision.0.checked_add(1) != Some(header.revision.0)
                {
                    return Err(integrity(
                        "Accepted revision disagrees with record history.",
                    ));
                }
                updated.next().filter(|change| change.client_ref.is_none())
            } else {
                let valid_case = if header.id.kind() == Kind::Case {
                    header.id == transaction.case_id && pending.is_empty()
                } else {
                    self.current(transaction.case_id).is_some()
                        || pending.contains_key(&transaction.case_id)
                };
                if header.revision.0 != 1
                    || !header.history.is_empty()
                    || header.created_at != header.recorded_at
                    || header.created_by != header.recorded_by
                    || !valid_case
                {
                    return Err(integrity("Accepted creation has invalid revision or case."));
                }
                created.next().filter(|change| {
                    change
                        .client_ref
                        .as_deref()
                        .is_none_or(|alias| aliases.insert(alias))
                })
            };
            if change.map(|change| (change.id, change.revision))
                != Some((header.id, header.revision.0))
            {
                return Err(integrity(
                    "Accepted record changes disagree with after-images.",
                ));
            }
            pending.insert(header.id, record);
        }
        if created.next().is_some() || updated.next().is_some() {
            return Err(integrity("Accepted changes have missing after-images."));
        }
        for record in &transaction.records {
            self.record(record.clone());
        }
        self.sequence = transaction.sequence;
        Ok(())
    }

    fn record(&mut self, record: Record) {
        self.derived = Derived::default();
        self.history
            .entry(record.header.id)
            .or_default()
            .push(record);
    }
}

pub struct Acceptance<'a> {
    pub sequence: u64,
    pub clock: &'a mut dyn Clock,
    pub ids: &'a mut dyn IdSource,
}

type FinalCheck = Box<dyn FnOnce(&State) -> Result<(), Failure>>;

/// Validate against the current state under the caller's commit lock.
/// The returned images may be persisted as one event and then applied with
/// `State::apply`. Validation leaves the input state untouched on success and failure.
pub fn validate_and_apply(
    state: &State,
    command: &Command,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
) -> Result<AcceptedTransaction, Failure> {
    validate_and_apply_with_observations(state, command, catalog, acceptance, &BTreeMap::new())
}

/// Attach trusted verification results per operation, computed from retained bytes.
pub fn validate_and_apply_with_observations(
    state: &State,
    command: &Command,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
    observations: &BTreeMap<usize, Result<ObservationProof, Failure>>,
) -> Result<AcceptedTransaction, Failure> {
    command.validate_shape()?;
    apply_operations(
        state,
        command,
        catalog,
        acceptance,
        observations,
        None,
        None,
    )
}

/// As [`validate_and_apply_with_observations`], with retained method pack
/// bytes so promotion gates can consult the case's pinned pack policy. Without
/// a source every pinned pack resolves as unavailable and supplies no policy.
pub fn validate_and_apply_with_packs(
    state: &State,
    command: &Command,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
    observations: &BTreeMap<usize, Result<ObservationProof, Failure>>,
    packs: &dyn PackSource,
) -> Result<AcceptedTransaction, Failure> {
    command.validate_shape()?;
    apply_operations(
        state,
        command,
        catalog,
        acceptance,
        observations,
        Some(packs),
        None,
    )
}

/// As [`validate_and_apply_with_packs`], with what the store knows about the
/// command's declared session, for write-side guardrails. Without it a
/// guardrail that requires a distinct context is unmet.
pub fn validate_and_apply_guarded(
    state: &State,
    command: &Command,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
    observations: &BTreeMap<usize, Result<ObservationProof, Failure>>,
    packs: &dyn PackSource,
    sessions: Option<&SessionEvidence>,
) -> Result<AcceptedTransaction, Failure> {
    command.validate_shape()?;
    apply_operations(
        state,
        command,
        catalog,
        acceptance,
        observations,
        Some(packs),
        sessions,
    )
}

/// Validate a transaction composed by a store pipeline rather than read from a
/// frontend. It may create its case and then record into it atomically; see
/// [`Command::validate_composed_shape`]. Every operation still runs through the
/// catalog with the same rules as a submitted command.
pub fn validate_and_apply_composed(
    state: &State,
    command: &Command,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
) -> Result<AcceptedTransaction, Failure> {
    command.validate_composed_shape()?;
    apply_operations(
        state,
        command,
        catalog,
        acceptance,
        &BTreeMap::new(),
        None,
        None,
    )
}

fn apply_operations(
    state: &State,
    command: &Command,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
    observations: &BTreeMap<usize, Result<ObservationProof, Failure>>,
    packs: Option<&dyn PackSource>,
    sessions: Option<&SessionEvidence>,
) -> Result<AcceptedTransaction, Failure> {
    if acceptance.sequence <= state.sequence {
        return Err(Failure::new(
            ErrorCode::IntegrityFailure,
            "Acceptance sequence must be newer than the loaded state.",
        ));
    }
    if let Some(case_id) = command.case_id
        && state.current(case_id).is_none()
    {
        return Err(
            Failure::new(ErrorCode::UnknownReference, "The case does not exist.").at("/case_id"),
        );
    }
    let mut preconditions = BTreeMap::new();
    for (index, precondition) in command.preconditions.iter().enumerate() {
        let path = format!("/preconditions/{index}");
        let current = state
            .current(precondition.id)
            .filter(|record| Some(record.header.case_id) == command.case_id)
            .ok_or_else(|| unknown_reference().at(&path))?;
        if preconditions
            .insert(precondition.id, precondition.revision)
            .is_some()
        {
            return Err(Failure::new(ErrorCode::InvalidInput, "Duplicate precondition.").at(path));
        }
        if current.header.revision.0 != precondition.revision {
            return Err(revision_conflict(*precondition, current.header.revision.0).at(path));
        }
    }

    let recorded_at = acceptance.clock.now_utc();
    let mut scratch = state.clone();
    let mut aliases = BTreeMap::new();
    let mut records = Vec::new();
    let mut created = Vec::new();
    let mut updated = Vec::new();
    let mut warnings = Vec::new();
    let mut final_checks = Vec::new();
    let mut case_id = command.case_id;
    for (index, operation) in command.operations.iter().enumerate() {
        let prefix = format!("/operations/{index}");
        if let Some(alias) = operation.client_ref.as_deref()
            && aliases.contains_key(alias)
        {
            return Err(
                Failure::new(ErrorCode::InvalidInput, "Duplicate same-batch alias.")
                    .at(format!("{prefix}/client_ref")),
            );
        }
        let descriptor = catalog
            .available_operation(&operation.op)
            .map_err(|error| prefix_path(error, &prefix, "/op"))?;
        let mut context = OperationContext {
            state: &mut scratch,
            command,
            case_id: &mut case_id,
            acceptance_sequence: acceptance.sequence,
            recorded_at: &recorded_at,
            ids: acceptance.ids,
            aliases: &aliases,
            preconditions: &preconditions,
            observation_proof: observations.get(&index),
            packs,
            sessions,
            images: Vec::new(),
            creation: None,
            final_checks: Vec::new(),
            warnings: Vec::new(),
        };
        descriptor
            .execute(&operation.payload, &mut context)
            .map_err(|error| prefix_path(error, &format!("{prefix}/payload"), ""))?;
        if let Some(extensions) = &operation.extensions {
            context
                .attach_extensions(extensions)
                .map_err(|error| prefix_path(error, &format!("{prefix}/extensions"), ""))?;
        }
        let OperationContext {
            images,
            creation,
            final_checks: checks,
            warnings: operation_warnings,
            ..
        } = context;
        if images.is_empty() {
            return Err(Failure::new(
                ErrorCode::IntegrityFailure,
                "Operation handler produced no record changes.",
            )
            .at(prefix));
        }
        if let Some(reference) = creation {
            if let Some(alias) = operation.client_ref.as_ref() {
                aliases.insert(alias.clone(), reference);
            }
            created.push(change(reference, operation.client_ref.clone()));
        } else if operation.client_ref.is_some() {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Only an operation that creates a record can declare a same-batch alias.",
            )
            .at(format!("{prefix}/client_ref")));
        }
        for image in images {
            let reference = image.reference();
            if reference.revision > 1 {
                updated.push(change(reference, None));
            }
            records.push(image);
        }
        final_checks.extend(checks.into_iter().map(|check| (index, check)));
        warnings.extend(operation_warnings);
    }
    scratch.sequence = acceptance.sequence;
    for (index, check) in final_checks {
        check(&scratch).map_err(|error| prefix_path(error, &format!("/operations/{index}"), ""))?;
    }
    let case_id = case_id.ok_or_else(|| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Case creation produced no case.",
        )
    })?;
    Ok(AcceptedTransaction {
        case_id,
        sequence: acceptance.sequence,
        records,
        created,
        updated,
        warnings,
    })
}

/// Restricted scratch-state interface for typed catalog handlers. Paths are
/// relative to the operation payload.
pub struct OperationContext<'a> {
    state: &'a mut State,
    command: &'a Command,
    case_id: &'a mut Option<Id>,
    acceptance_sequence: u64,
    recorded_at: &'a UtcTimestamp,
    ids: &'a mut dyn IdSource,
    aliases: &'a BTreeMap<String, RevisionRef>,
    preconditions: &'a BTreeMap<Id, u64>,
    observation_proof: Option<&'a Result<ObservationProof, Failure>>,
    packs: Option<&'a dyn PackSource>,
    sessions: Option<&'a SessionEvidence>,
    images: Vec<Record>,
    creation: Option<RevisionRef>,
    final_checks: Vec<FinalCheck>,
    warnings: Vec<String>,
}

impl OperationContext<'_> {
    pub fn case_id(&self) -> Option<Id> {
        *self.case_id
    }

    pub fn acceptance_sequence(&self) -> u64 {
        self.acceptance_sequence
    }

    /// The attributed command actor. This is a claim, not authentication.
    pub fn actor(&self) -> &Actor {
        &self.command.actor
    }

    /// The command's self-reported actor descriptor. A claim, not authentication.
    pub fn actor_descriptor(&self) -> Option<&crate::header::ActorDescriptor> {
        self.command.actor_descriptor.as_ref()
    }

    pub fn state(&self) -> &State {
        self.state
    }

    pub fn observation_proof(&self) -> Result<Option<&ObservationProof>, Failure> {
        self.observation_proof
            .map(|proof| proof.as_ref().map_err(Clone::clone))
            .transpose()
    }

    /// Retained method pack bytes, when the caller supplied them.
    pub fn pack_source(&self) -> Option<&dyn PackSource> {
        self.packs
    }

    /// The command's self-reported session label, when it declared one.
    pub fn session(&self) -> Option<&crate::inherited_trust::Session> {
        self.command.session.as_ref()
    }

    /// The command's human confirmation claim. Not authenticated.
    pub fn human_confirmation(&self) -> Option<&crate::guardrails::HumanConfirmation> {
        self.command.human_confirmation.as_ref()
    }

    /// Session reads the store supplied for write-side guardrails.
    pub fn session_evidence(&self) -> Option<&SessionEvidence> {
        self.sessions
    }

    pub fn preconditions(&self) -> &BTreeMap<Id, u64> {
        self.preconditions
    }

    pub fn current(&self, id: Id) -> Option<&Record> {
        self.state
            .current(id)
            .filter(|record| Some(record.header.case_id) == *self.case_id)
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        let case = *self.case_id;
        self.state
            .records()
            .filter(move |record| Some(record.header.case_id) == case)
    }

    pub fn warn(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    pub fn exact(&self, reference: RevisionRef) -> Option<&Record> {
        self.state
            .revision(reference)
            .filter(|record| Some(record.header.case_id) == *self.case_id)
    }

    pub fn resolve(
        &self,
        reference: &Reference,
        kind: Kind,
        path: &str,
    ) -> Result<RevisionRef, Failure> {
        let resolved = self.resolve_id(reference, path)?;
        if resolved.id.kind() != kind {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                format!("Expected a {kind} record reference."),
            )
            .at(path));
        }
        self.exact(resolved)
            .map(|_| resolved)
            .ok_or_else(|| unknown_reference().at(path))
    }

    pub fn resolve_reference(
        &self,
        reference: &Reference,
        path: &str,
    ) -> Result<RevisionRef, Failure> {
        let resolved = self.resolve_id(reference, path)?;
        self.exact(resolved)
            .map(|_| resolved)
            .ok_or_else(|| unknown_reference().at(path))
    }

    fn resolve_id(&self, reference: &Reference, path: &str) -> Result<RevisionRef, Failure> {
        match reference {
            Reference::Existing(reference) => Ok(*reference),
            Reference::Client(alias) => self
                .aliases
                .get(&alias.client_ref)
                .copied()
                .ok_or_else(|| unknown_reference().at(path)),
        }
    }
    pub fn create(&mut self, kind: Kind, body: Value) -> Result<RevisionRef, Failure> {
        self.create_classified(kind, body, Classification::default())
    }

    pub fn create_classified(
        &mut self,
        kind: Kind,
        body: Value,
        classification: Classification,
    ) -> Result<RevisionRef, Failure> {
        let invalid = || {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "Invalid creation in operation handler.",
            )
        };
        if self.creation.is_some()
            || !body.is_object()
            || (kind == Kind::Case) != self.case_id.is_none()
        {
            return Err(invalid());
        }
        let id = self.ids.next_id(kind).map_err(|_| invalid())?;
        if id.kind() != kind || self.state.current(id).is_some() {
            return Err(invalid());
        }
        let case_id = *self.case_id.get_or_insert(id);
        let record = Record {
            header: RecordHeader {
                id,
                case_id,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: self.command.actor.clone(),
                actor_descriptor_self_reported: self.command.actor_descriptor.clone(),
                created_at: self.recorded_at.clone(),
                recorded_at: self.recorded_at.clone(),
                recorded_by: self.command.actor.clone(),
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: self.acceptance_sequence,
                revision: Revision(1),
                history: Vec::new(),
                classification,
                provenance: Default::default(),
                extensions: BTreeMap::new(),
            },
            body,
        };
        let reference = record.reference();
        self.creation = Some(reference);
        self.push(record);
        Ok(reference)
    }

    /// Replace a record only at its expected current revision.
    pub fn update(&mut self, expected: RevisionRef, body: Value) -> Result<RevisionRef, Failure> {
        let old = self.current(expected.id).ok_or_else(unknown_reference)?;
        if old.header.revision.0 != expected.revision {
            return Err(revision_conflict(expected, old.header.revision.0));
        }
        let next = expected
            .revision
            .checked_add(1)
            .filter(|_| body.is_object())
            .ok_or_else(|| {
                Failure::new(
                    ErrorCode::IntegrityFailure,
                    "Invalid revision or body in operation handler.",
                )
            })?;
        let mut header = old.header.clone();
        header.history.push(header.revision);
        header.revision = Revision(next);
        header.acceptance_sequence = self.acceptance_sequence;
        header.recorded_at = self.recorded_at.clone();
        header.recorded_by = self.command.actor.clone();
        let new = Record { header, body };
        let reference = new.reference();
        self.push(new);
        Ok(reference)
    }

    /// Store an operation's validated pack extensions on the header of the
    /// record it created, or of the single record it revised.
    fn attach_extensions(&mut self, requested: &BTreeMap<String, Value>) -> Result<(), Failure> {
        let target = crate::pack_extensions::target(
            self.creation.map(|reference| reference.id),
            &self.images,
        )?;
        let case_id = self
            .case_id
            .ok_or_else(|| Failure::new(ErrorCode::IntegrityFailure, "Extensions need a case."))?;
        let index = self
            .images
            .iter()
            .rposition(|record| record.header.id == target)
            .expect("the target is one of the operation's images");
        let extensions = crate::pack_extensions::apply(
            self.state,
            case_id,
            self.packs,
            &self.images[index],
            requested,
        )?;
        self.images[index].header.extensions = extensions.clone();
        self.state.derived = Derived::default();
        if let Some(latest) = self
            .state
            .history
            .get_mut(&target)
            .and_then(|revisions| revisions.last_mut())
        {
            latest.header.extensions = extensions;
        }
        Ok(())
    }

    pub fn check_final(&mut self, check: impl FnOnce(&State) -> Result<(), Failure> + 'static) {
        self.final_checks.push(Box::new(check));
    }

    fn push(&mut self, record: Record) {
        self.state.record(record.clone());
        self.images.push(record);
    }
}

fn unknown_reference() -> Failure {
    Failure::new(
        ErrorCode::UnknownReference,
        "Expected an existing revision in this case or an alias created by an earlier operation.",
    )
}

fn revision_conflict(expected: RevisionRef, revision: u64) -> Failure {
    let mut error = Failure::new(
        ErrorCode::RevisionConflict,
        "The expected revision is no longer current.",
    );
    error.error.expected = Some(json!(expected));
    error.error.actual = Some(json!(RevisionRef {
        revision,
        ..expected
    }));
    error
}

fn prefix_path(mut error: Failure, prefix: &str, fallback: &str) -> Failure {
    let path = error
        .error
        .path
        .as_deref()
        .filter(|path| !path.is_empty())
        .unwrap_or(fallback);
    error.error.path = Some(format!("{prefix}{path}"));
    error
}

fn change(reference: RevisionRef, client_ref: Option<String>) -> RecordChange {
    RecordChange {
        client_ref,
        id: reference.id,
        revision: reference.revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{OperationDescriptor, standard_catalog};
    use schemars::JsonSchema;
    use uuid::Uuid;

    #[derive(Default)]
    struct Ids(u64);

    impl IdSource for Ids {
        fn next_uuid(&mut self) -> Uuid {
            self.0 += 1;
            format!("018d87fc-5234-7000-8000-{:012x}", self.0)
                .parse()
                .unwrap()
        }
    }

    struct FixedTime;

    impl Clock for FixedTime {
        fn now_utc(&mut self) -> UtcTimestamp {
            "2026-11-22T08:12:44.000Z".parse().unwrap()
        }
    }

    fn command(case_id: Option<Id>, operations: Value) -> Command {
        serde_json::from_value(json!({
            "schema_version": "inv.command.v0-alpha",
            "command_id": "bounded-test",
            "actor": "agent:test",
            "case_id": case_id,
            "operations": operations,
            "preconditions": []
        }))
        .unwrap()
    }

    fn validate(
        state: &State,
        command: &Command,
        catalog: &Catalog,
        ids: &mut Ids,
    ) -> Result<AcceptedTransaction, Failure> {
        validate_and_apply(
            state,
            command,
            catalog,
            Acceptance {
                sequence: state.sequence() + 1,
                clock: &mut FixedTime,
                ids,
            },
        )
    }

    fn create_case(state: &mut State, catalog: &Catalog, ids: &mut Ids) -> Id {
        let payload = catalog.get("case.create").unwrap().example().unwrap();
        let command = command(None, json!([{ "op": "case.create", "payload": payload }]));
        let accepted = validate(state, &command, catalog, ids).unwrap();
        state.apply(&accepted).unwrap();
        accepted.case_id
    }

    fn question(alias: &str, parent: Option<Value>) -> Value {
        json!({
            "op": "question.create",
            "client_ref": alias,
            "payload": {"wording": "What explains the latency variation?", "importance": "normal", "parent": parent},
        })
    }

    #[test]
    fn resolves_aliases_in_order_and_replays_the_exact_images() {
        let catalog = standard_catalog();
        let mut ids = Ids::default();
        let mut state = State::default();
        let case_id = create_case(&mut state, &catalog, &mut ids);
        let command = command(
            Some(case_id),
            json!([
                question("parent", None),
                question("child", Some(json!({"client_ref": "parent"}))),
            ]),
        );
        let accepted = validate(&state, &command, &catalog, &mut ids).unwrap();
        assert_eq!(ids.0, 3);
        assert_eq!(state.records().count(), 1);
        assert_eq!(
            accepted.records[1].body["parent"],
            json!(accepted.records[0].reference())
        );
        let bytes = serde_json::to_vec(&accepted).unwrap();
        let decoded: AcceptedTransaction = serde_json::from_slice(&bytes).unwrap();
        let mut damaged = decoded.clone();
        damaged.records[1].header.revision = Revision(2);
        let original = state.clone();
        assert_eq!(
            state.apply(&damaged).unwrap_err().error.code,
            ErrorCode::IntegrityFailure
        );
        assert_eq!(state, original);
        state.apply(&decoded).unwrap();
        assert_eq!(state.records().count(), 3);
        assert_eq!(
            state.current(accepted.created[1].id),
            Some(&accepted.records[1])
        );
    }

    #[test]
    fn failed_batches_identify_alias_and_cross_case_errors() {
        let catalog = standard_catalog();
        let mut ids = Ids::default();
        let mut state = State::default();
        let case_id = create_case(&mut state, &catalog, &mut ids);
        let original = state.clone();
        for (operations, path) in [
            (
                json!([
                    question("first", None),
                    question("second", Some(json!({"client_ref": "missing"})))
                ]),
                "/operations/1/payload/parent",
            ),
            (
                json!([
                    question("first", Some(json!({"client_ref": "future"}))),
                    question("future", None)
                ]),
                "/operations/0/payload/parent",
            ),
            (
                json!([question("first", None), question("first", None)]),
                "/operations/1/client_ref",
            ),
        ] {
            let error = validate(
                &state,
                &command(Some(case_id), operations),
                &catalog,
                &mut ids,
            )
            .unwrap_err();
            assert_eq!(error.error.path.as_deref(), Some(path));
            assert_eq!(state, original);
        }
        let other = create_case(&mut state, &catalog, &mut ids);
        let accepted = validate(
            &state,
            &command(Some(other), json!([question("other", None)])),
            &catalog,
            &mut ids,
        )
        .unwrap();
        state.apply(&accepted).unwrap();
        let parent = json!(accepted.records[0].reference());
        let error = validate(
            &state,
            &command(Some(case_id), json!([question("first", Some(parent))])),
            &catalog,
            &mut ids,
        )
        .unwrap_err();
        assert_eq!(error.error.code, ErrorCode::UnknownReference);
        assert_eq!(
            error.error.path.as_deref(),
            Some("/operations/0/payload/parent")
        );
    }

    #[derive(Deserialize, JsonSchema)]
    struct Edit {
        expected: RevisionRef,
        allowed: bool,
        #[serde(default)]
        promotion: bool,
    }

    #[test]
    fn verifies_expected_revisions_and_checks_promotions_against_final_state() {
        let standard = standard_catalog();
        let mut ids = Ids::default();
        let mut state = State::default();
        let case_id = create_case(&mut state, &standard, &mut ids);
        let original = state.current(case_id).unwrap().clone();
        let mut info = *standard.get("case.create").unwrap().info();
        info.name = "test.edit";
        let edits = Catalog::new([OperationDescriptor::active(
            info,
            json!({}),
            |_: &Edit| Ok(()),
            |edit: Edit, context| {
                let updated = context.update(edit.expected, json!({"allowed": edit.allowed}))?;
                if edit.promotion {
                    context.check_final(move |state| {
                        if state.current(updated.id).unwrap().body["allowed"] == true {
                            Ok(())
                        } else {
                            Err(Failure::new(
                                ErrorCode::GateFailed,
                                "Final state blocks promotion.",
                            ))
                        }
                    });
                }
                Ok(())
            },
        )])
        .unwrap();
        let edit = |revision, allowed, promotion| {
            json!({
                "op": "test.edit",
                "payload": {"expected": {"id": case_id, "revision": revision}, "allowed": allowed, "promotion": promotion},
            })
        };
        let mut first = command(Some(case_id), json!([edit(1, true, true)]));
        first.preconditions = vec![RevisionRef {
            id: case_id,
            revision: 2,
        }];
        let stale = validate(&state, &first, &edits, &mut ids).unwrap_err();
        assert_eq!(stale.error.code, ErrorCode::RevisionConflict);
        assert_eq!(stale.error.path.as_deref(), Some("/preconditions/0"));

        let batch = command(
            Some(case_id),
            json!([edit(1, true, true), edit(2, false, false)]),
        );
        let refused = validate(&state, &batch, &edits, &mut ids).unwrap_err();
        assert_eq!(refused.error.code, ErrorCode::GateFailed);
        assert_eq!(refused.error.path.as_deref(), Some("/operations/0"));
        assert_eq!(state.current(case_id), Some(&original));

        let batch = command(
            Some(case_id),
            json!([edit(1, true, true), edit(2, true, false)]),
        );
        let accepted = validate(&state, &batch, &edits, &mut ids).unwrap();
        let mut damaged = accepted.clone();
        damaged.records[1].header.history.clear();
        assert_eq!(
            state.apply(&damaged).unwrap_err().error.code,
            ErrorCode::IntegrityFailure
        );
        assert_eq!(state.current(case_id), Some(&original));
        state.apply(&accepted).unwrap();
        let current = &state.current(case_id).unwrap().header;
        assert_eq!(current.revision, Revision(3));
        assert_eq!(current.history, [Revision(1), Revision(2)]);
        assert_eq!(current.created_by, original.header.created_by);
        assert_eq!(current.created_at, original.header.created_at);
        assert_eq!(state.revision(original.reference()), Some(&original));
        assert_eq!(ids.0, 1);
    }
}
