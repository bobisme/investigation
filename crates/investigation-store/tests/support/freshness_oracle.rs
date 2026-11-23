//! An independent, deliberately simple recomputation of effective freshness.
//!
//! It shares nothing with `investigation_core::freshness` except plain state
//! accessors: it finds citations by walking record bodies for exact revision
//! references, classifies each one with its own table, judges changes with its
//! own substantive field lists, and re-derives what a validation review saw
//! from the journal history at the review's sequence. Its rules are written
//! from the plan (§5.5, §5.7, §14.3, §15.3), not from the index.
//!
//! Rules, in order:
//! 1. A citation of an exact revision is *changed* when the record's current
//!    revision differs in substance (per-kind field lists below), became ended
//!    (withdrawn, retracted, retired, rejected, superseded), recorded a new
//!    withdrawal or newly raised source concern, changed an assumption's
//!    disposition, or made a measurement invalid. Ending and use changes do not
//!    count for a cited *subject*: a judgment about a subject stays one.
//! 2. A changed citation needs review (the reader must look at the new
//!    revision; the new revision's own state is not inherited). An unchanged
//!    *basis* citation, or one whose later revisions are all immaterial, passes
//!    on the cited record's current effective freshness; subjects and context
//!    (questions, option scopes) are checked only for change.
//!    2a. Acknowledgement (§5.7, W06b.16): a use change, meaning a partial
//!    withdrawal or a newly raised source concern, carries the sequence from
//!    which it was in effect. A revision accepted at or after it read the
//!    material with the change in effect: the change does not count for it,
//!    and a basis citation then passes on as if unchanged. The change still
//!    reaches every dependent, which judges it against its own acceptance.
//!    An entire withdrawal is never acknowledged.
//! 3. An open critical objection disputes its subject.
//! 4. A submitted validation review is also judged against what it recorded
//!    it saw (its scoped sets, with each member's substance at the review's
//!    sequence): an open objection on the subject it did not see disputes it;
//!    a changed open-objection, material-assumption, eligible-option or
//!    active-hard-criterion set needs review; and so does a record now
//!    required to read the subject (its citation closure, read through later
//!    revisions that are all immaterial, plus material assumptions naming any
//!    record in it) that the review did not read. These checks hold whether
//!    or not the subject changed since the review.
//!
//! Three readings were once open questions where the index differed; rules
//! version 6 adopted the plain reading of each (W10.05e): an objection's
//! resolution rests on its `resolution_evidence_refs`, an option's
//! disposition rests on its `criterion_ref`, and rule 4 survives a change to
//! the reviewed subject. The tests that pinned the divergences now pin the
//! agreement.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use investigation_core::{
    command::RevisionRef,
    identity::{Id, Kind},
    state::{Record, State},
};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Current,
    NeedsReview,
    Disputed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub level: Level,
    pub needs_review: bool,
    pub disputed_by: BTreeSet<Id>,
    /// Needs review for a cause no later reading acknowledges.
    lasting_review: bool,
    /// Use changes (a partial withdrawal or a newly raised source concern)
    /// at or upstream of this record: the changed Source or Artifact and the
    /// sequence from which the change was in effect. Each reader judges them
    /// against its own acceptance (rule 2a), so they pass on whether or not
    /// this record acknowledged them.
    use_changes: BTreeSet<(Id, u64)>,
    /// (rule, detail) for every local cause and inherited state; for
    /// failure reports and coverage counts only.
    pub why: Vec<(&'static str, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The dependent relies on the cited record and inherits its freshness.
    Basis,
    /// A judgment about the cited record: only a substantive change counts.
    Subject,
    /// A question or scope: any change counts, nothing is inherited.
    Context,
    /// Not a dependency (lineage, reverse links, acknowledgments).
    Ignored,
}

#[derive(Clone, Debug)]
pub struct Citation {
    pub path: String,
    pub target: RevisionRef,
    pub role: Role,
    /// Part of what a reviewer of the citing record must read.
    pub read: bool,
}

pub struct Oracle<'a> {
    state: &'a State,
    case: Id,
    memo: RefCell<BTreeMap<RevisionRef, Verdict>>,
    visiting: RefCell<BTreeSet<RevisionRef>>,
}

impl<'a> Oracle<'a> {
    pub fn new(state: &'a State, case: Id) -> Self {
        Self {
            state,
            case,
            memo: RefCell::default(),
            visiting: RefCell::default(),
        }
    }

    fn current(&self, id: Id) -> Option<&'a Record> {
        self.state
            .current(id)
            .filter(|record| record.header.case_id == self.case)
    }

    /// Every revision reference in the body, classified.
    pub fn citations(&self, record: &Record) -> Vec<Citation> {
        let mut found = Vec::new();
        find_refs(&record.body, String::new(), &mut found);
        found
            .into_iter()
            .map(|(path, target)| {
                let (role, read) = role(record.header.id.kind(), &path);
                Citation {
                    path,
                    target,
                    role,
                    read,
                }
            })
            .collect()
    }

    /// Records the overview counts: current and not ended.
    pub fn in_use(&self, record: &Record) -> bool {
        ended(record).is_none()
    }

    /// Effective freshness of any revision of a record in this case.
    pub fn verdict(&self, reference: RevisionRef) -> Verdict {
        if let Some(found) = self.memo.borrow().get(&reference) {
            return found.clone();
        }
        assert!(
            self.visiting.borrow_mut().insert(reference),
            "oracle: circular basis at {reference:?}"
        );
        let record = self
            .state
            .revision(reference)
            .unwrap_or_else(|| panic!("oracle: unknown revision {reference:?}"));
        let mut verdict = Verdict {
            level: Level::Current,
            needs_review: false,
            disputed_by: BTreeSet::new(),
            lasting_review: false,
            use_changes: BTreeSet::new(),
            why: Vec::new(),
        };
        // When this revision's author read what it cites.
        let read_at = record.header.acceptance_sequence;
        let mine = own_option(record);
        for citation in self.citations(record) {
            if citation.role == Role::Ignored {
                continue;
            }
            let Some(now) = self.current(citation.target.id) else {
                verdict.review("missing", citation.path.clone());
                continue;
            };
            let cited = self
                .state
                .revision(citation.target)
                .expect("cited revision exists");
            let reader_option = if citation.target.id.kind() == Kind::Criterion {
                mine
            } else {
                None
            };
            let detail = format!(
                "{}: {} r{} -> r{}",
                citation.path, citation.target.id, citation.target.revision, now.header.revision.0
            );
            match change(cited, now, citation.role, reader_option) {
                // Rule 2a: a use change this revision was accepted after is
                // one its author read with the change in effect.
                Some((rule, Some(since))) => {
                    verdict.use_changes.insert((citation.target.id, since));
                    if since > read_at {
                        verdict
                            .why
                            .push((rule, format!("needs_review: {detail} (since {since})")));
                    } else {
                        verdict.why.push((
                            "acknowledged",
                            format!("{rule}: {detail} (since {since}, read at {read_at})"),
                        ));
                        if citation.role == Role::Basis {
                            self.inherit(&mut verdict, &citation, now);
                        }
                    }
                }
                Some((rule, None)) => verdict.review(rule, detail),
                None if citation.role == Role::Basis => self.inherit(&mut verdict, &citation, now),
                None => {}
            }
        }
        for objection in self.open_objections(record.header.id) {
            if self.current(objection).unwrap().body["severity"] == "critical" {
                verdict.dispute(objection, "critical_objection", String::new());
            }
        }
        if record.header.id.kind() == Kind::Review
            && record.body["kind"] == "validation"
            && record.body["status"] == "submitted"
        {
            self.judge_review(record, &mut verdict);
        }
        let pending: Vec<_> = verdict
            .use_changes
            .iter()
            .filter(|(_, since)| *since > read_at)
            .copied()
            .collect();
        if !pending.is_empty() {
            verdict.needs_review = true;
            verdict.level = verdict.level.max(Level::NeedsReview);
            verdict.why.push((
                "use_change_after_reading",
                format!("needs_review: {pending:?} after {read_at}"),
            ));
        } else if !verdict.use_changes.is_empty() {
            verdict.why.push((
                "acknowledged",
                format!("use changes {:?} read at {read_at}", verdict.use_changes),
            ));
        }
        self.visiting.borrow_mut().remove(&reference);
        self.memo.borrow_mut().insert(reference, verdict.clone());
        verdict
    }

    /// Rule 2: pass on the cited record's current effective freshness.
    fn inherit(&self, verdict: &mut Verdict, citation: &Citation, now: &Record) {
        let inherited = self.verdict(now.reference());
        if inherited.lasting_review {
            verdict.review(
                "inherited",
                format!("{} -> {} needs review", citation.path, citation.target.id),
            );
        }
        verdict.use_changes.extend(inherited.use_changes);
        for objection in inherited.disputed_by {
            verdict.dispute(
                objection,
                "inherited",
                format!("{} -> {}", citation.path, citation.target.id),
            );
        }
    }

    /// The revision of `id` that was current at `sequence`.
    fn at(&self, id: Id, sequence: u64) -> Option<&'a Record> {
        self.state
            .history(id)
            .filter(|record| record.header.acceptance_sequence <= sequence)
            .last()
    }

    /// Current records of one kind in this case.
    fn current_of(&self, kind: Kind) -> impl Iterator<Item = &'a Record> + '_ {
        self.state.records().filter(move |record| {
            record.header.case_id == self.case && record.header.id.kind() == kind
        })
    }

    fn open_objections(&self, subject: Id) -> BTreeSet<Id> {
        self.current_of(Kind::Objection)
            .filter(|objection| {
                objection.body["status"] == "open"
                    && ref_id(&objection.body["subject_ref"]) == Some(subject)
            })
            .map(|objection| objection.header.id)
            .collect()
    }

    /// Material, not superseded assumptions naming `subject`, with their substance.
    fn material_assumptions(&self, subject: Id) -> BTreeMap<Id, Value> {
        self.current_of(Kind::Assumption)
            .filter(|assumption| {
                let body = &assumption.body;
                body["materiality"] == "material"
                    && body["status"] != "superseded"
                    && body["affected_refs"]
                        .as_array()
                        .is_some_and(|refs| refs.iter().any(|r| ref_id(r) == Some(subject)))
            })
            .map(|assumption| (assumption.header.id, substance(assumption, None)))
            .collect()
    }

    fn eligible_options(&self, question: Id) -> BTreeMap<Id, Value> {
        self.current_of(Kind::Option)
            .filter(|option| {
                option.body["lifecycle"] == "eligible"
                    && ref_id(&option.body["question_ref"]) == Some(question)
            })
            .map(|option| (option.header.id, substance(option, None)))
            .collect()
    }

    fn hard_criteria(&self, question: Id, option: Option<Id>) -> BTreeMap<Id, Value> {
        self.current_of(Kind::Criterion)
            .filter(|criterion| {
                let body = &criterion.body;
                body["kind"] == "hard"
                    && body["lifecycle"] == "active"
                    && ref_id(&body["question_ref"]) == Some(question)
                    && covers(body, option)
            })
            .map(|criterion| (criterion.header.id, substance(criterion, None)))
            .collect()
    }

    /// Rule 4: what a validation review saw at its own sequence against now.
    fn judge_review(&self, review: &Record, verdict: &mut Verdict) {
        let then = review.header.acceptance_sequence;
        let subject = ref_id(&review.body["subject_ref"]).expect("review subject");
        let Some(now_subject) = self.current(subject) else {
            return;
        };
        let seen = |rule: &str| -> BTreeMap<Id, Value> {
            review.body["memberships"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|membership| membership["rule"] == rule)
                .flat_map(|membership| membership["members"].as_array().into_iter().flatten())
                .filter_map(|member| serde_json::from_value::<Id>(member["id"].clone()).ok())
                .map(|id| {
                    (
                        id,
                        substance(self.at(id, then).expect("member existed"), None),
                    )
                })
                .collect()
        };
        let opened_then: BTreeSet<Id> = seen("open_objections").into_keys().collect();
        let opened_now = self.open_objections(subject);
        for raised in opened_now.difference(&opened_then) {
            verdict.dispute(*raised, "objection_raised_after_review", String::new());
        }
        if opened_then.difference(&opened_now).next().is_some() {
            verdict.review("objection_closed_since_review", String::new());
        }
        if seen("material_assumptions") != self.material_assumptions(subject) {
            verdict.review("material_assumptions_changed", String::new());
        }
        if subject.kind() == Kind::Decision {
            let question = ref_id(&now_subject.body["question_ref"]).expect("decision question");
            let chosen = ref_id(&now_subject.body["choice"]["option_ref"]);
            if seen("eligible_options") != self.eligible_options(question) {
                verdict.review("eligible_options_changed", String::new());
            }
            if seen("active_hard_criteria") != self.hard_criteria(question, chosen) {
                verdict.review("hard_criteria_changed", String::new());
            }
        }
        let read: BTreeSet<Id> = review.body["read_set"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(ref_id)
            .collect();
        // Rule 4: a record newly required to read the subject that the review
        // did not read, whatever else changed in the closure.
        for required in self.required(now_subject) {
            if required != subject && !read.contains(&required) {
                verdict.review("basis_expanded", format!("{required} was not read"));
            }
        }
    }

    /// The records a reviewer of `root` must read: its read citations,
    /// transitively, plus current material assumptions naming any of them
    /// (and what those cite). A citation is read at the current revision
    /// when every later revision is immaterial to a plain reader (rule 1),
    /// else at the cited revision.
    fn required(&self, root: &Record) -> BTreeSet<Id> {
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(record) = pending.pop() {
            if !seen.insert(record.header.id) {
                continue;
            }
            for citation in self.citations(record) {
                if !citation.read {
                    continue;
                }
                let Some(cited) = self.state.revision(citation.target) else {
                    continue;
                };
                let read = self
                    .current(citation.target.id)
                    .filter(|now| change(cited, now, Role::Basis, None).is_none())
                    .unwrap_or(cited);
                pending.push(read);
            }
            for assumption in self.material_assumptions(record.header.id).keys() {
                pending.extend(self.current(*assumption));
            }
        }
        seen
    }
}

impl Verdict {
    fn review(&mut self, rule: &'static str, detail: String) {
        self.needs_review = true;
        self.lasting_review = true;
        self.level = self.level.max(Level::NeedsReview);
        self.why.push((rule, format!("needs_review: {detail}")));
    }

    fn dispute(&mut self, objection: Id, rule: &'static str, detail: String) {
        self.disputed_by.insert(objection);
        self.level = Level::Disputed;
        self.why
            .push((rule, format!("disputed by {objection}: {detail}")));
    }
}

fn ref_id(value: &Value) -> Option<Id> {
    serde_json::from_value::<RevisionRef>(value.clone())
        .ok()
        .map(|reference| reference.id)
}

/// Every `{"id": …, "revision": …}` object in a body, with its dotted path
/// (array positions elided).
fn find_refs(value: &Value, path: String, out: &mut Vec<(String, RevisionRef)>) {
    match value {
        Value::Object(object) => {
            if object.len() == 2
                && object.contains_key("id")
                && object.contains_key("revision")
                && let Ok(reference) = serde_json::from_value::<RevisionRef>(value.clone())
            {
                out.push((path, reference));
                return;
            }
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                find_refs(child, child_path, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                find_refs(item, path.clone(), out);
            }
        }
        _ => {}
    }
}

/// The oracle's own dependency table. An unlisted reference is a harness
/// error: every reference a generated record can hold must be classified.
fn role(kind: Kind, path: &str) -> (Role, bool) {
    use Role::{Basis, Context, Ignored, Subject};
    let role = match (kind, path) {
        (Kind::Artifact, "source" | "transforms.input") => Basis,
        (Kind::Observation, "basis.artifact_ref" | "run_ref") => Basis,
        // A correction names the revision it corrects: lineage, not basis.
        (Kind::Observation, "corrects") => Ignored,
        (Kind::Hypothesis, "question_ref") => Context,
        (Kind::Hypothesis, "assumption_refs" | "evidence_refs") => Basis,
        (Kind::Hypothesis, "rival_refs" | "alternative_to") => Ignored,
        (Kind::Assessment, "subject_ref") => Subject,
        (Kind::Assessment, "evidence_refs" | "assumption_refs") => Basis,
        (Kind::Conclusion, "support_refs" | "contradiction_refs") => Basis,
        // A validated claim relies on its validation review; the review is
        // not something a reviewer of the claim reads.
        (Kind::Conclusion | Kind::Decision, "review_ref") => return (Basis, false),
        (Kind::Option, "question_ref") => Context,
        // A disposition rests on the criterion it names.
        (Kind::Option, "criterion_ref") => Basis,
        (Kind::Criterion, "question_ref" | "option_refs") => Context,
        // An assumption names what it affects; it does not rely on it.
        (Kind::Assumption, "affected_refs") => Ignored,
        (Kind::Assumption, "evidence_refs") => Basis,
        (
            Kind::Comparison,
            "option_ref" | "criterion_ref" | "evidence_refs" | "assumption_refs",
        ) => Basis,
        (Kind::Decision, "question_ref") => Context,
        (
            Kind::Decision,
            "choice.option_ref"
            | "compared_alternatives"
            | "comparison_refs"
            | "hard_conditions.criterion_ref"
            | "evidence_refs"
            | "assumption_refs",
        ) => Basis,
        // What an acceptance acknowledged is a record of that moment.
        (Kind::Decision, path) if path.starts_with("acceptance_challenges.") => Ignored,
        (Kind::Review, "subject_ref") => return (Subject, false),
        (Kind::Review, "read_set" | "materials_examined" | "evidence_refs" | "assumption_refs") => {
            return (Basis, false);
        }
        (Kind::Review, "objection_refs") => return (Context, false),
        (Kind::Objection, "subject_ref") => return (Subject, false),
        (Kind::Objection, "evidence_refs.value") => return (Basis, false),
        (Kind::Objection, "review_ref") => return (Context, false),
        // A resolution rests on the evidence it cites (§14.3).
        (Kind::Objection, "resolution_evidence_refs") => return (Basis, false),
        (Kind::Annotation, "subject_refs" | "content.artifact_ref") => return (Basis, false),
        (Kind::Question, "parent") => Context,
        (Kind::Question, "evidence_refs") => Basis,
        (kind, path) => panic!("oracle: unclassified reference {kind:?}.{path}"),
    };
    (role, role != Ignored)
}

/// Why a record is out of evidentiary use, by the oracle's own list.
fn ended(record: &Record) -> Option<&'static str> {
    let body = &record.body;
    let status = body["status"].as_str().unwrap_or("");
    let lifecycle = body["lifecycle"].as_str().unwrap_or("");
    let ended = match record.header.id.kind() {
        Kind::Source | Kind::Artifact => {
            status == "withdrawn"
                || matches!(
                    body["availability"].as_str(),
                    Some("missing" | "intentionally_removed")
                )
        }
        Kind::Observation => status == "retracted",
        Kind::Comparison => body["retracted"] == true,
        Kind::Assessment => matches!(status, "retracted" | "superseded"),
        Kind::Conclusion => matches!(status, "retracted" | "rejected" | "superseded"),
        Kind::Criterion => lifecycle == "retired",
        Kind::Hypothesis => matches!(
            lifecycle,
            "rejected" | "rejected_within_scope" | "retired" | "superseded"
        ),
        Kind::Option => matches!(lifecycle, "withdrawn" | "superseded"),
        Kind::Assumption => matches!(status, "rejected" | "superseded"),
        Kind::Decision => lifecycle == "superseded",
        Kind::Review => matches!(status, "superseded" | "withdrawn"),
        Kind::Objection => status == "withdrawn",
        _ => false,
    };
    ended.then_some("ended")
}

/// The fields a reader relies on, per kind. Everything else (status,
/// lifecycle, reasons, review pointers, judgments) is metadata.
fn substantive_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Source => &[
            "identity",
            "producer",
            "publisher",
            "acquisition",
            "access",
            "verification_limits",
        ],
        Kind::Artifact => &[
            "digest",
            "size_bytes",
            "format",
            "source",
            "transforms",
            "role",
        ],
        Kind::Observation => &[
            "value",
            "basis",
            "extraction_method",
            "evidence_kind",
            "quality",
            "limitations",
            "observed_at",
            "variables_changed",
            "validity",
            "measurement",
            "run_ref",
        ],
        Kind::Question => &["wording", "scope", "context", "importance", "parent"],
        Kind::Hypothesis => &[
            "statement",
            "applicability",
            "possible_falsifiers",
            "question_ref",
            "assumption_refs",
            "evidence_refs",
            "rival_refs",
        ],
        Kind::Assessment => &[
            "subject_ref",
            "evidence_refs",
            "assumption_refs",
            "relation",
            "rationale",
            "rival_expectations",
            "confidence",
            "likelihood",
        ],
        Kind::Conclusion => &[
            "statement",
            "scope",
            "significance",
            "support_refs",
            "contradiction_refs",
            "caveats",
            "causal_factors",
            "causal_test",
            "claim_form",
            "confidence",
            "likelihood",
        ],
        Kind::Option => &[
            "question_ref",
            "description",
            "context",
            "feasibility",
            "dependencies",
            "tradeoff_notes",
        ],
        Kind::Criterion => &[
            "name",
            "kind",
            "question_ref",
            "option_refs",
            "scale",
            "preferred_direction",
            "threshold",
            "weighting_rationale",
        ],
        // The disposition and its reasons are part of what an assumption says.
        Kind::Assumption => &[
            "claim",
            "consequence",
            "materiality",
            "affected_refs",
            "status",
            "rationale",
            "evidence_refs",
        ],
        Kind::Comparison => &[
            "option_ref",
            "criterion_ref",
            "outcome",
            "method",
            "value",
            "basis",
            "evidence_refs",
            "assumption_refs",
            "limitations",
            "uncertainty",
            "rationale",
            "absence",
        ],
        Kind::Decision => &[
            "question_ref",
            "choice",
            "compared_alternatives",
            "comparison_refs",
            "hard_conditions",
            "evidence_refs",
            "assumption_refs",
            "tradeoffs",
            "consequences",
            "reopen_trigger",
            "expected_outcome",
            "check_when",
            "reversal_conditions",
        ],
        Kind::Objection => &[
            "subject_ref",
            "statement",
            "scope",
            "severity",
            "evidence_refs",
            "review_ref",
        ],
        Kind::Annotation => &["subject_refs", "content", "role"],
        Kind::Review => &[
            "kind",
            "subject_ref",
            "basis_digest",
            "read_set",
            "materials_examined",
            "method",
            "verdict",
            "caveats",
            "memberships",
            "objection_refs",
        ],
        other => panic!("oracle: no substantive fields for {other:?}"),
    }
}

/// The substance of a record for a reader. A reader with its own option sees
/// a criterion's option scope only as whether it still covers that option.
fn substance(record: &Record, reader_option: Option<Id>) -> Value {
    let mut out = serde_json::Map::new();
    for field in substantive_fields(record.header.id.kind()) {
        out.insert((*field).to_owned(), record.body[*field].clone());
    }
    if let Some(option) = reader_option {
        out.insert(
            "option_refs".into(),
            Value::Bool(covers(&record.body, Some(option))),
        );
    }
    Value::Object(out)
}

/// Whether a criterion body applies to `option` (`None`: to the question).
fn covers(criterion: &Value, option: Option<Id>) -> bool {
    match (&criterion["option_refs"], option) {
        (Value::Null, _) | (_, None) => true,
        (Value::Array(refs), Some(option)) => refs.iter().any(|r| ref_id(r) == Some(option)),
        _ => false,
    }
}

/// The option a comparison or decision is about.
fn own_option(record: &Record) -> Option<Id> {
    match record.header.id.kind() {
        Kind::Comparison => ref_id(&record.body["option_ref"]),
        Kind::Decision => ref_id(&record.body["choice"]["option_ref"]),
        _ => None,
    }
}

fn withdrawals(record: &Record) -> &[Value] {
    record.body["withdrawals"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

/// Dimensions whose latest source judgment is a concern, with the sequence
/// at which that judgment was accepted.
fn concerns(record: &Record) -> BTreeMap<&'static str, u64> {
    let mut latest: BTreeMap<&'static str, (String, u64)> = BTreeMap::new();
    for judgment in record.body["source_assessment"]["judgments"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let at = judgment["accepted_at_sequence"]
            .as_u64()
            .expect("a judgment records its sequence");
        for dimension in ["authenticity", "reliability", "currency", "origin"] {
            if let Some(value) = judgment[dimension]["judgment"].as_str() {
                latest.insert(dimension, (value.to_owned(), at));
            }
        }
    }
    latest
        .into_iter()
        .filter(|(dimension, (value, _))| {
            matches!(
                (*dimension, value.as_str()),
                ("authenticity", "doubtful" | "not_authentic")
                    | ("reliability", "unreliable")
                    | ("currency", "outdated" | "superseded")
            )
        })
        .map(|(dimension, (_, at))| (dimension, at))
        .collect()
}

/// Rule 2a: a new withdrawal or a newly raised concern between two revisions
/// of a Source or Artifact, with the sequence from which it was in effect.
/// The sequence is `None` when no later reading makes the material usable
/// again: an entire withdrawal, or any withdrawal of entirely withdrawn
/// material. A partial withdrawal names its part in words, so every reader
/// before it needs review; a reader after it read it in effect.
fn use_change(cited: &Record, now: &Record) -> Option<(&'static str, Option<u64>)> {
    let before = withdrawals(cited);
    let new = withdrawals(now).get(before.len()..).unwrap_or_default();
    if !new.is_empty()
        && (now.body["status"] == "withdrawn"
            || new
                .iter()
                .any(|withdrawal| withdrawal["scope"]["extent"] != "partial"))
    {
        return Some(("withdrawal", None));
    }
    let withdrawn_at = new
        .iter()
        .map(|withdrawal| {
            withdrawal["accepted_at_sequence"]
                .as_u64()
                .expect("a withdrawal records its sequence")
        })
        .max();
    let earlier = concerns(cited);
    let raised_at = concerns(now)
        .into_iter()
        .filter(|(dimension, _)| !earlier.contains_key(dimension))
        .map(|(_, at)| at)
        .max();
    match (withdrawn_at, raised_at) {
        (None, None) => None,
        (Some(_), _) => Some(("withdrawal", withdrawn_at.max(raised_at))),
        (None, Some(_)) => Some(("source_concern", raised_at)),
    }
}

/// Rule 1: whether the change from `cited` to `now` matters to the reader,
/// under which rule, and (rule 2a) from when a use change was in effect.
fn change(
    cited: &Record,
    now: &Record,
    role: Role,
    reader_option: Option<Id>,
) -> Option<(&'static str, Option<u64>)> {
    if cited.header.revision == now.header.revision {
        return None;
    }
    if now.header.id.kind() == Kind::Assumption && cited.body["status"] != now.body["status"] {
        return Some(("assumption_disposition", None));
    }
    if role != Role::Subject {
        if let Some(found) = use_change(cited, now) {
            return Some(found);
        }
        if ended(now).is_some() && ended(cited).is_none() {
            return Some(("ended", None));
        }
    }
    if now.header.id.kind() == Kind::Observation
        && now.body["validity"] == "invalid"
        && cited.body["validity"] != "invalid"
    {
        return Some(("invalid_measurement", None));
    }
    (substance(cited, reader_option) != substance(now, reader_option))
        .then_some(("substantive_change", None))
}
