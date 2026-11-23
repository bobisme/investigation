use super::*;
use crate::{
    header::{Actor, Classification, Provenance, RecordHeader, Revision, SchemaVersion},
    state::AcceptedTransaction,
    wire::RecordChange,
};
use serde_json::json;

const CASE: &str = "case_018d87fc-5234-7000-8000-000000000001";
const OTHER_CASE: &str = "case_018d87fc-5234-7000-8000-0000000000ff";

fn id(value: &str) -> Id {
    value.parse().unwrap()
}

fn rid(prefix: &str, n: u32) -> Id {
    id(&format!("{prefix}_018d87fc-5234-7000-8000-{n:012x}"))
}

fn session(value: &str) -> Session {
    value.parse().unwrap()
}

/// One case built transaction by transaction, one record revision each.
struct Journal {
    state: State,
    ledger: SessionLedger,
}

impl Journal {
    fn new() -> Self {
        let mut journal = Self {
            state: State::default(),
            ledger: SessionLedger::new(),
        };
        journal.add(id(CASE), json!({"title":"Case"}), None);
        journal
    }

    /// Commit `body` as the next revision of `record`, with the session
    /// reads the store would stamp for a declared session.
    fn add(&mut self, record: Id, body: Value, reads: Option<SessionReads>) -> RevisionRef {
        let sequence = self.state.sequence() + 1;
        let actor: Actor = "agent:writer".parse().unwrap();
        let time: crate::header::UtcTimestamp = "2026-11-22T00:00:00.0Z".parse().unwrap();
        let previous = self.state.current(record).cloned();
        let header = match previous {
            Some(old) => {
                let mut header = old.header.clone();
                header.history.push(header.revision);
                header.revision = Revision(header.revision.0 + 1);
                header.acceptance_sequence = sequence;
                header
            }
            None => RecordHeader {
                id: record,
                case_id: id(CASE),
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor.clone(),
                actor_descriptor_self_reported: None,
                created_at: time.clone(),
                recorded_at: time,
                recorded_by: actor,
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: sequence,
                revision: Revision(1),
                history: vec![],
                classification: Classification::default(),
                provenance: Provenance::default(),
                extensions: Default::default(),
            },
        };
        let change = RecordChange {
            client_ref: None,
            id: record,
            revision: header.revision.0,
        };
        let updated = header.revision.0 > 1;
        let written = Record { header, body };
        let reference = written.reference();
        self.state
            .apply(&AcceptedTransaction {
                case_id: id(CASE),
                sequence,
                records: vec![written],
                created: if updated {
                    vec![]
                } else {
                    vec![change.clone()]
                },
                updated: if updated { vec![change] } else { vec![] },
                warnings: vec![],
            })
            .unwrap();
        if let Some(reads) = reads {
            self.ledger.insert(sequence, reads);
        }
        reference
    }

    fn inherited(&self, record: Id) -> InheritedTrust {
        Resolver::new(&self.state, &self.ledger).inherited(self.state.current(record).unwrap())
    }
}

fn read_of(artifact: Id, revision: u64) -> SessionRead {
    SessionRead {
        case_id: id(CASE),
        artifact: RevisionRef {
            id: artifact,
            revision,
        },
    }
}

#[test]
fn a_record_with_no_declared_input_is_unknown() {
    let mut journal = Journal::new();
    let question = rid("q", 1);
    journal.add(question, json!({"text":"Why?"}), None);
    let trust = journal.inherited(question);
    assert_eq!(trust.level, TrustLevel::Unknown);
    assert!(trust.inputs.is_empty());
    assert_eq!(trust.undeclared_sessions(), 1);

    // A declared session that read nothing through the tool is still no input.
    let note = rid("ann", 2);
    journal.add(
        note,
        json!({"text":"note"}),
        Some(SessionReads::new(session("s-1"), vec![], 0)),
    );
    let trust = journal.inherited(note);
    assert_eq!(trust.level, TrustLevel::Unknown);
    assert_eq!(trust.undeclared_sessions(), 0);
}

#[test]
fn an_evidence_read_in_the_declared_session_sets_untrusted_external() {
    let mut journal = Journal::new();
    let artifact = rid("art", 3);
    journal.add(
        artifact,
        json!({"source":{"id":rid("src", 4),"revision":1}}),
        None,
    );
    let question = rid("q", 1);
    let question_ref = journal.add(question, json!({"text":"Why?"}), None);
    let synopsis = rid("ann", 2);
    journal.add(
        synopsis,
        json!({"role":"synopsis","subjects":[question_ref],"text":"Summary"}),
        Some(SessionReads::new(
            session("s-1"),
            vec![read_of(artifact, 1)],
            0,
        )),
    );
    let trust = journal.inherited(synopsis);
    assert_eq!(trust.level, TrustLevel::UntrustedExternal);
    assert_eq!(trust.session_reads, vec![read_of(artifact, 1)]);
    assert_eq!(trust.session_reads_total(), 1);
    assert_eq!(
        trust.inputs,
        vec![DeclaredInput {
            id: question,
            revision: Some(1),
            level: TrustLevel::Unknown,
            outside_case: false,
        }]
    );
}

#[test]
fn cited_evidence_is_inherited_through_authored_records() {
    let mut journal = Journal::new();
    let observation = rid("obs", 5);
    let observation_ref = journal.add(observation, json!({"statement":"x"}), None);
    let assessment = rid("assess", 6);
    let assessment_ref = journal.add(
        assessment,
        json!({"observation_ref":observation_ref,"rationale":"r"}),
        None,
    );
    let hypothesis = rid("hyp", 7);
    let hypothesis_ref = journal.add(hypothesis, json!({"statement":"h"}), None);
    let speculative = rid("concl", 8);
    journal.add(
        speculative,
        json!({"claim":"c","support_refs":[hypothesis_ref]}),
        None,
    );
    assert_eq!(
        journal.inherited(speculative).level,
        TrustLevel::Unknown,
        "authored inputs alone never make a record trusted"
    );
    let grounded = rid("concl", 9);
    journal.add(
        grounded,
        json!({"claim":"c","support_refs":[hypothesis_ref, assessment_ref]}),
        None,
    );
    let trust = journal.inherited(grounded);
    assert_eq!(trust.level, TrustLevel::UntrustedExternal);
    assert!(
        trust
            .inputs
            .iter()
            .any(|input| input.id == assessment && input.level == TrustLevel::UntrustedExternal)
    );
    // Bare IDs in *_refs fields count too, read at their current revision.
    let lesson_holder = rid("retro", 10);
    journal.add(
        lesson_holder,
        json!({"lessons":[{"text":"l","support_refs":[observation.to_string()]}]}),
        None,
    );
    let trust = journal.inherited(lesson_holder);
    assert_eq!(trust.level, TrustLevel::UntrustedExternal);
    assert_eq!(trust.inputs[0].revision, None);
}

#[test]
fn an_amendment_keeps_the_lowest_level_of_every_revision() {
    let mut journal = Journal::new();
    let artifact = rid("art", 3);
    journal.add(artifact, json!({}), None);
    let conclusion = rid("concl", 8);
    journal.add(
        conclusion,
        json!({"claim":"first"}),
        Some(SessionReads::new(
            session("s-1"),
            vec![read_of(artifact, 1)],
            0,
        )),
    );
    journal.add(
        conclusion,
        json!({"claim":"amended"}),
        Some(SessionReads::new(session("s-2"), vec![], 0)),
    );
    let trust = journal.inherited(conclusion);
    assert_eq!(trust.level, TrustLevel::UntrustedExternal);
    assert_eq!(trust.sessions.len(), 2);
    assert_eq!(trust.sessions[0].reads_total, 1);
    assert_eq!(trust.sessions[1].session, Some(session("s-2")));
}

#[test]
fn cycles_and_other_cases_terminate_without_inventing_trust() {
    let mut journal = Journal::new();
    let first = rid("ann", 11);
    let second = rid("ann", 12);
    journal.add(first, json!({"related_ref":second.to_string()}), None);
    journal.add(second, json!({"related_ref":first.to_string()}), None);
    assert_eq!(journal.inherited(first).level, TrustLevel::Unknown);

    let elsewhere = id(&format!("retro_{}", &OTHER_CASE[5..]));
    let foreign_artifact = rid("art", 99);
    let consult = rid("ann", 13);
    journal.add(
        consult,
        json!({"lesson":{"retrospective_ref":{"id":elsewhere,"revision":1}}}),
        None,
    );
    let trust = journal.inherited(consult);
    assert_eq!(trust.level, TrustLevel::Unknown);
    assert!(trust.inputs[0].outside_case);
    let cites_foreign = rid("ann", 14);
    journal.add(
        cites_foreign,
        json!({"evidence_refs":[{"id":foreign_artifact,"revision":1}]}),
        None,
    );
    let trust = journal.inherited(cites_foreign);
    assert_eq!(trust.level, TrustLevel::UntrustedExternal);
    assert!(trust.inputs[0].outside_case);
}

#[test]
fn session_reads_are_distinct_bounded_and_counted() {
    let artifact = rid("art", 3);
    let mut reads: Vec<SessionRead> = (1..=(MAX_SESSION_READS as u64 + 5))
        .map(|revision| read_of(artifact, revision))
        .collect();
    reads.push(read_of(artifact, 1));
    let summary = SessionReads::new(session("s-1"), reads, 0);
    assert_eq!(summary.reads.len(), MAX_SESSION_READS);
    assert_eq!(summary.reads_total, MAX_SESSION_READS as u64 + 5);
    assert_eq!(summary.lowest, Some(TrustLevel::UntrustedExternal));
    assert_eq!(SessionReads::new(session("s-1"), vec![], 0).lowest, None);
    let unreadable = SessionReads::new(session("s-1"), vec![], 1);
    assert_eq!(unreadable.reads_total, 1);
    assert_eq!(unreadable.lowest, Some(TrustLevel::UntrustedExternal));
}

#[test]
fn sessions_are_bounded_inert_labels() {
    assert!("agent-run:2026.11.23_a".parse::<Session>().is_ok());
    for bad in ["", "two words", "a/b", "line\nbreak", &"x".repeat(129)] {
        assert!(bad.parse::<Session>().is_err(), "{bad:?}");
    }
    assert!(TrustLevel::UntrustedExternal < TrustLevel::Unknown);
    assert_eq!(
        serde_json::to_value(TrustLevel::UntrustedExternal).unwrap(),
        json!("untrusted_external")
    );
}
