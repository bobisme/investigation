//! Authored synopses: attributed `annotation.add` text with `role: synopsis`.
//!
//! A synopsis is editorial material bound to one exact subject revision. It is
//! never evidence: it has no evidentiary standing, is never a source hit and
//! cannot be cited where evidence is required. Its text is authored data,
//! never instructions.
//!
//! Whether it still describes its subject is read from the dependency index.
//! The annotation cites its subject revision through an `Inherit` edge, so a
//! material revision of the subject, or any change that makes the subject need
//! review (a withdrawn source, a revised criterion), makes the synopsis
//! `stale` with the same reason chain. A later synopsis of the same subject
//! replaces an earlier one; the earlier one stays in history. Nothing here
//! rewrites a record.

use serde::Serialize;

use crate::{
    command::RevisionRef,
    freshness::{Freshness, FreshnessIndex, FreshnessReason},
    header::{Actor, UtcTimestamp},
    identity::{Id, Kind},
    search_text::TextLabel,
    state::{Record, State},
};

/// The annotation role of an authored synopsis.
pub const SYNOPSIS_ROLE: &str = "synopsis";

/// Largest synopsis text in UTF-8 bytes, the bound of a summary-depth read.
pub const SYNOPSIS_MAX_BYTES: usize = 700;

/// Whether an authored synopsis may still be read as describing its subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SynopsisState {
    /// The newest synopsis of its subject, and nothing it rests on changed.
    Current,
    /// The subject changed materially or needs review since the synopsis was
    /// written. Reads fall back to the deterministic projection.
    Stale,
    /// A later synopsis of the same subject replaces this one.
    Replaced,
}

impl SynopsisState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Replaced => "replaced",
        }
    }
}

/// The binding and standing of one authored synopsis at one snapshot.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SynopsisStatus {
    pub annotation_ref: RevisionRef,
    /// The exact subject revision the synopsis was written against.
    pub subject_ref: RevisionRef,
    /// The subject's current revision in this snapshot, if it still exists.
    pub subject_current: Option<RevisionRef>,
    pub state: SynopsisState,
    pub replaced_by: Option<RevisionRef>,
    pub recorded_by: Actor,
    pub recorded_at: UtcTimestamp,
    /// Effective freshness of the annotation and why it is not current.
    pub freshness: Freshness,
    pub reasons: Vec<FreshnessReason>,
    pub label: TextLabel,
    pub counts_as_source_hit: bool,
    pub evidentiary_status: &'static str,
    pub content_trust: &'static str,
}

/// Whether a record is an authored synopsis annotation.
pub fn is_synopsis(record: &Record) -> bool {
    record.header.id.kind() == Kind::Annotation
        && record.body.get("role").and_then(|role| role.as_str()) == Some(SYNOPSIS_ROLE)
}

/// The authored text of a synopsis annotation.
pub fn text(record: &Record) -> Option<&str> {
    is_synopsis(record)
        .then(|| record.body["content"]["text"].as_str())
        .flatten()
}

fn subject(record: &Record) -> Option<RevisionRef> {
    serde_json::from_value(record.body["subject_refs"].get(0)?.clone()).ok()
}

/// Current synopsis annotations of one subject, oldest first.
fn of_subject(state: &State, case_id: Id, subject_id: Id) -> Vec<&Record> {
    let mut found: Vec<&Record> = state
        .records()
        .filter(|record| {
            record.header.case_id == case_id
                && is_synopsis(record)
                && subject(record).is_some_and(|subject| subject.id == subject_id)
        })
        .collect();
    found.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    found
}

/// The standing of one synopsis annotation. `index` must be built from
/// `state` for the annotation's case. `None` for any other record.
pub fn status(
    state: &State,
    index: &FreshnessIndex,
    annotation: &Record,
) -> Option<SynopsisStatus> {
    let subject_ref = subject(annotation).filter(|_| is_synopsis(annotation))?;
    let later = of_subject(state, annotation.header.case_id, subject_ref.id)
        .into_iter()
        .rfind(|other| {
            (other.header.acceptance_sequence, other.header.id)
                > (annotation.header.acceptance_sequence, annotation.header.id)
        })
        .map(Record::reference);
    let effective = index.assess(state, annotation);
    let state_word = match (later, effective.freshness) {
        (Some(_), _) => SynopsisState::Replaced,
        (None, Freshness::Current) => SynopsisState::Current,
        (None, _) => SynopsisState::Stale,
    };
    Some(SynopsisStatus {
        annotation_ref: annotation.reference(),
        subject_ref,
        subject_current: state.current(subject_ref.id).map(Record::reference),
        state: state_word,
        replaced_by: later,
        recorded_by: annotation.header.recorded_by.clone(),
        recorded_at: annotation.header.recorded_at.clone(),
        freshness: effective.freshness,
        reasons: effective.reasons,
        label: TextLabel::EditorialSummary,
        counts_as_source_hit: false,
        evidentiary_status: "none",
        content_trust: TextLabel::EditorialSummary.content_trust(),
    })
}

/// Every synopsis of a subject, newest first, so the first entry is the one a
/// reader would see. Older entries are `replaced`.
pub fn for_subject(state: &State, index: &FreshnessIndex, subject_id: Id) -> Vec<SynopsisStatus> {
    let mut found: Vec<_> = of_subject(state, index.case_id(), subject_id)
        .into_iter()
        .filter_map(|record| status(state, index, record))
        .collect();
    found.reverse();
    found
}

/// Counts of the synopses of one case, by state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SynopsisCounts {
    pub current: usize,
    pub stale: usize,
    pub replaced: usize,
    /// Stale synopses in ID order.
    pub stale_refs: Vec<RevisionRef>,
}

impl SynopsisCounts {
    pub fn total(&self) -> usize {
        self.current + self.stale + self.replaced
    }
}

/// Counts over every synopsis in the indexed case that `visible` accepts.
pub fn case_counts(
    state: &State,
    index: &FreshnessIndex,
    visible: impl Fn(&Record) -> bool,
) -> SynopsisCounts {
    let mut counts = SynopsisCounts::default();
    let case_id = index.case_id();
    for record in state
        .records()
        .filter(|record| record.header.case_id == case_id && is_synopsis(record))
        .filter(|record| visible(record))
    {
        let Some(status) = status(state, index, record) else {
            continue;
        };
        match status.state {
            SynopsisState::Current => counts.current += 1,
            SynopsisState::Stale => {
                counts.stale += 1;
                counts.stale_refs.push(status.annotation_ref);
            }
            SynopsisState::Replaced => counts.replaced += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::SystemIdSource,
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
        wire::Failure,
    };

    struct Case {
        state: State,
        case: Option<Id>,
        commands: u64,
    }

    impl Case {
        fn new() -> Self {
            let mut case = Self {
                state: State::default(),
                case: None,
                commands: 0,
            };
            let accepted = case
                .transact(vec![json!({"op":"case.create","payload":{
                    "title":"Queue choice","mode":"record"}})])
                .unwrap();
            case.case = Some(accepted.case_id);
            case
        }

        fn transact(&mut self, operations: Vec<Value>) -> Result<AcceptedTransaction, Failure> {
            self.commands += 1;
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version":"inv.command.v1","command_id":format!("synopsis-{}", self.commands),
                "case_id":self.case,"actor":"agent:summarizer","preconditions":[],"operations":operations,
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            Ok(accepted)
        }

        fn one(&mut self, op: &str, payload: Value) -> RevisionRef {
            self.transact(vec![json!({"op":op,"payload":payload})])
                .unwrap()
                .records[0]
                .reference()
        }

        fn synopsis(&mut self, subject: RevisionRef, text: &str) -> Result<RevisionRef, Failure> {
            self.transact(vec![json!({"op":"annotation.add","payload":{
                "subject_refs":[subject],"content":{"kind":"text","text":text},"role":"synopsis"}})])
                .map(|accepted| accepted.records[0].reference())
        }

        fn index(&self) -> FreshnessIndex {
            FreshnessIndex::build(&self.state, self.case.unwrap())
        }

        fn status(&self, annotation: RevisionRef) -> SynopsisStatus {
            let record = self.state.revision(annotation).unwrap();
            status(&self.state, &self.index(), record).unwrap()
        }
    }

    fn latency(question: RevisionRef, limit: &str) -> Value {
        json!({
            "name":"Latency","kind":"hard","preferred_direction":"lower",
            "scale":{"anchors":[],"description":"p99 latency","unit":"ms"},
            "scope":{"kind":"all_options","question_ref":question},
            "threshold":{"kind":"measured","operator":"at_most","value":{"decimal":limit,"unit":"ms"}}
        })
    }

    #[test]
    fn a_criterion_revision_makes_the_bound_synopsis_stale_and_history_keeps_it() {
        let mut case = Case::new();
        let question = case.one(
            "question.create",
            json!({"importance":"normal","wording":"Which queue should carry the jobs?"}),
        );
        let criterion = case.one("criterion.create", latency(question, "25"));
        let synopsis = case
            .synopsis(
                criterion,
                "The limit is a p99 of 25 ms under projected load.",
            )
            .unwrap();
        let record = case.state.revision(synopsis).unwrap();
        assert_eq!(record.body["role"], "synopsis");
        assert_eq!(record.body["evidentiary_status"], "none");
        assert_eq!(record.header.recorded_by.to_string(), "agent:summarizer");
        let fresh = case.status(synopsis);
        assert_eq!(fresh.state, SynopsisState::Current);
        assert_eq!(fresh.subject_ref, criterion);
        assert_eq!(fresh.label, TextLabel::EditorialSummary);
        assert!(!fresh.counts_as_source_hit);

        let revised = case.one(
            "criterion.amend",
            json!({"criterion_ref":criterion,"reason":"The service level changed.",
                "replacement":latency(question, "40")}),
        );
        assert_eq!(revised.revision, 2);
        let stale = case.status(synopsis);
        assert_eq!(stale.state, SynopsisState::Stale);
        assert_eq!(stale.subject_ref, criterion);
        assert_eq!(stale.subject_current, Some(revised));
        assert_eq!(stale.reasons[0].cause, revised);
        assert_eq!(
            serde_json::to_value(&stale.reasons[0].trigger).unwrap()["kind"],
            "material_revision"
        );
        // A synopsis must bind the subject's current revision.
        let refused = case.synopsis(criterion, "Still 25 ms.").unwrap_err();
        assert_eq!(
            refused.error.path.as_deref(),
            Some("/operations/0/payload/subject_refs/0")
        );

        // A new synopsis replaces the stale one; history retains both.
        let newer = case.synopsis(revised, "The limit is now 40 ms.").unwrap();
        let index = case.index();
        let all = for_subject(&case.state, &index, criterion.id);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].annotation_ref, newer);
        assert_eq!(all[0].state, SynopsisState::Current);
        assert_eq!(all[1].annotation_ref, synopsis);
        assert_eq!(all[1].state, SynopsisState::Replaced);
        assert_eq!(all[1].replaced_by, Some(newer));
        assert_eq!(
            text(case.state.revision(synopsis).unwrap()),
            Some("The limit is a p99 of 25 ms under projected load.")
        );
        let counts = case_counts(&case.state, &index, |_| true);
        assert_eq!((counts.current, counts.stale, counts.replaced), (1, 0, 1));
    }

    #[test]
    fn a_synopsis_is_bounded_bound_to_one_subject_and_never_evidence() {
        let mut case = Case::new();
        let question = case.one(
            "question.create",
            json!({"importance":"normal","wording":"Which queue should carry the jobs?"}),
        );
        let other = case.one(
            "question.create",
            json!({"importance":"normal","wording":"Which region should host it?"}),
        );
        let refuse = |case: &mut Case, op: &str, payload: Value| {
            case.transact(vec![json!({"op":op,"payload":payload})])
                .unwrap_err()
        };
        let two = refuse(
            &mut case,
            "annotation.add",
            json!({"subject_refs":[question, other],"role":"synopsis",
                "content":{"kind":"text","text":"Two questions."}}),
        );
        assert_eq!(
            two.error.path.as_deref(),
            Some("/operations/0/payload/subject_refs")
        );
        let long = refuse(
            &mut case,
            "annotation.add",
            json!({"subject_refs":[question],"role":"synopsis",
                "content":{"kind":"text","text":"é".repeat(SYNOPSIS_MAX_BYTES)}}),
        );
        assert_eq!(
            long.error.path.as_deref(),
            Some("/operations/0/payload/content/text")
        );
        let synopsis = case.synopsis(question, "Two queues remain.").unwrap();
        let nested = case
            .synopsis(synopsis, "A synopsis of a synopsis.")
            .unwrap_err();
        assert_eq!(
            nested.error.path.as_deref(),
            Some("/operations/0/payload/subject_refs/0")
        );

        // Evidence gates accept no annotation, so a synopsis never corroborates.
        let hypothesis = case.one("hypothesis.create", json!({
            "question_ref":question,"statement":"The managed queue meets the limit.",
            "applicability":"Projected load","assumption_refs":[],"possible_falsifiers":["p99 above 25 ms"],"rival_refs":[]
        }));
        let assessed = refuse(
            &mut case,
            "assessment.add",
            json!({"subject_ref":hypothesis,"evidence_refs":[synopsis],"relation":"supports",
                "rationale":"The synopsis says so."}),
        );
        assert_eq!(
            assessed.error.path.as_deref(),
            Some("/operations/0/payload/evidence_refs/0"),
            "{assessed:?}"
        );
        let concluded = refuse(
            &mut case,
            "conclusion.create",
            json!({"statement":"The managed queue meets the limit.","scope":"Projected load.",
                "significance":"Informs the choice.","support_refs":[synopsis],
                "contradiction_refs":[],"caveats":[]}),
        );
        assert_eq!(
            concluded.error.path.as_deref(),
            Some("/operations/0/payload/support_refs/0"),
            "{concluded:?}"
        );
    }
}
