//! `review.submit(kind=blindspot)` through the real operation catalog.

use serde_json::{Value, json};

use super::*;
use crate::{
    blindspot::AuditResponse,
    catalog::standard_catalog,
    command::Command,
    header::SystemClock,
    identity::SystemIdSource,
    review::ReviewSubmit,
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
};

struct Case {
    state: State,
    case: Option<Id>,
}

impl Case {
    fn new() -> Self {
        let mut case = Self {
            state: State::default(),
            case: None,
        };
        case.state.note_verified_snapshot(case.snapshot()).unwrap();
        case.one(
            "case.create",
            json!({"title": "Blindspot review", "mode": "record"}),
        );
        case
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sequence: self.state.sequence(),
            head_hash: format!("sha256:{:064x}", self.state.sequence() + 7)
                .parse()
                .unwrap(),
        }
    }

    fn run(&mut self, ops: Vec<Value>) -> Result<AcceptedTransaction, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("command-{sequence}"),
            "case_id": self.case,
            "actor": "agent:reviewer",
            "preconditions": [],
            "operations": ops,
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
        self.state.apply(&accepted).unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        self.case.get_or_insert(accepted.case_id);
        Ok(accepted)
    }

    fn one(&mut self, name: &str, payload: Value) -> RevisionRef {
        self.run(vec![op(name, payload)]).unwrap().records[0].reference()
    }

    fn audit(&self, subject: Option<Id>) -> AuditResponse {
        audit(
            &self.state,
            &self.snapshot(),
            self.case.unwrap(),
            &AuditRequest { subject },
        )
        .unwrap()
    }

    /// Stores a record image directly, for a body no operation in this
    /// fixture creates (a supporting Source).
    fn image(&mut self, kind: Kind, body: Value) -> RevisionRef {
        let case = self.case.unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = self.state.current(case).unwrap().header.clone();
        header.acceptance_sequence = sequence;
        header.id = crate::identity::IdSource::next_id(&mut SystemIdSource, kind).unwrap();
        header.revision.0 = 1;
        header.history.clear();
        let record = Record { header, body };
        let reference = record.reference();
        let change = crate::wire::RecordChange {
            client_ref: None,
            id: reference.id,
            revision: reference.revision,
        };
        self.state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records: vec![record],
                created: vec![change],
                updated: vec![],
                warnings: vec![],
            })
            .unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        reference
    }

    fn get(&self, reference: RevisionRef) -> &Record {
        self.state.revision(reference).unwrap()
    }
}

/// The failure path within the operation payload.
fn payload_path(path: &str) -> &str {
    path.split_once("/payload").map_or(path, |(_, rest)| rest)
}

fn op(name: &str, payload: Value) -> Value {
    json!({"op": name, "payload": payload})
}

/// A causal candidate Conclusion with no rival account, which the
/// `conclusion.causal_claim_without_rival_account` check reports.
struct Fixture {
    question: RevisionRef,
    hypothesis: RevisionRef,
    conclusion: RevisionRef,
}

fn fixture(case: &mut Case) -> Fixture {
    let question = case.one(
        "question.create",
        json!({"importance":"normal","wording":"Why do requests fail?"}),
    );
    let hypothesis = case.one(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"The scheduler setting causes failures.",
            "possible_falsifiers":["Failures persist without the setting."],"rival_refs":[],"assumption_refs":[]}),
    );
    let conclusion = case.one(
        "conclusion.create",
        json!({"statement":"The scheduler setting causes the failures.","scope":"The staged workload.",
            "significance":"Decides whether to change the setting.","support_refs":[],"contradiction_refs":[],
            "caveats":["One workload."],"causal_factors":[{"hypothesis_ref":hypothesis,"role":"trigger"}]}),
    );
    Fixture {
        question,
        hypothesis,
        conclusion,
    }
}

fn context() -> Value {
    json!({
        "saw_original_conclusion":{"state":"known","value":true},
        "gathered_new_evidence":{"state":"known","value":false},
        "method_or_source_changes":[],"activity":"Challenge the recorded causal claim.",
        "dependencies":"shared","dependence_basis":"Same recorded data as the investigator."
    })
}

fn review(subject: RevisionRef, response: &AuditResponse, read_set: Value) -> Value {
    json!({
        "kind":"blindspot","subject_ref":subject,"audited_snapshot":response.snapshot,
        "read_set":read_set,"method":"Read the audit, then look for unrecorded mechanisms.",
        "scope_examined":"The causal conclusion and the hypothesis it names; not the whole case.",
        "checks_considered":["rival mechanisms","measurement failure"],"gaps":[],
        "no_gap_reason":null,"evidence_refs":[],"assumption_refs":[],
        "limitations":["No new external evidence was gathered."],"verdict":"inconclusive",
        "context_isolation":"same_session_self_review","context":context(),
        "audit":{"check_version":response.check_version,"check_set_digest":response.check_set_digest}
    })
}

fn causal_gap(response: &AuditResponse) -> String {
    response
        .checks
        .iter()
        .find(|finding| finding.check_id == "conclusion.causal_claim_without_rival_account")
        .expect("the causal check reports the conclusion")
        .gap_id
        .clone()
}

#[test]
fn zero_gaps_record_the_audited_scope_and_snapshot_with_a_bounded_reason() {
    let mut case = Case::new();
    let question = case.one(
        "question.create",
        json!({"importance":"normal","wording":"Which queue?"}),
    );
    let response = case.audit(None);
    assert!(response.checks.is_empty());
    let case_ref = case.state.current(case.case.unwrap()).unwrap().reference();
    let mut payload = review(case_ref, &response, json!([question]));
    payload["verdict"] = json!("no_gap_identified_within_scope");
    payload["no_gap_reason"] =
        json!("Only one open question is recorded; nothing is concluded yet.");
    let accepted = case.run(vec![op("review.submit", payload)]).unwrap();
    assert!(accepted.updated.is_empty());
    // Only the independence notices: no undispositioned gap is reported.
    assert_eq!(
        accepted.warnings,
        crate::review_independence::submission_warnings(
            crate::review::ContextIsolation::SameSessionSelfReview,
            &serde_json::from_value(context()).unwrap()
        )
    );
    let body = &accepted.records[0].body;
    assert_eq!(body["kind"], "blindspot");
    assert_eq!(body["audited_snapshot"], json!(response.snapshot));
    assert_eq!(body["read_set"], json!([question]));
    assert_eq!(body["audit"]["scope"], "case");
    assert_eq!(body["audit"]["reported_gap_ids"], json!([]));
    assert_eq!(
        body["audit"]["check_set_digest"],
        json!(current_check_set_digest())
    );
    assert!(body.get("gap_dispositions").is_none() && body.get("novel_gaps").is_none());
}

#[test]
fn audit_gaps_and_novel_gaps_are_recorded_with_exact_ids_and_same_batch_promotion() {
    let mut case = Case::new();
    let f = fixture(&mut case);
    let before = case.audit(Some(f.conclusion.id));
    let gap_id = causal_gap(&before);
    assert_eq!(before.scope.subject_ref, Some(f.conclusion));

    let mut payload = review(f.conclusion, &before, json!([f.question, f.hypothesis]));
    payload["verdict"] = json!("gaps_identified");
    payload["audit"]["subject_ref"] = json!(f.conclusion);
    payload["gap_dispositions"] = json!([{
        "gap_id": gap_id, "disposition": "accepted",
        "reason": "No rival mechanism was considered for the failures.",
        "related_refs": [f.hypothesis], "record_refs": [{"client_ref": "rival-question"}]
    }]);
    payload["novel_gaps"] = json!([
        {"statement": "The client retry timer may produce the same failures under load.",
         "uncertainty": "Unknown whether retries were enabled in the staged run.",
         "related_refs": [f.question], "record_refs": [{"client_ref": "rival-question"}, {"client_ref": "timer"}]},
        {"statement": "The failure counter may double-count timeouts.", "related_refs": [f.hypothesis]}
    ]);
    let accepted = case
        .run(vec![
            json!({"op":"question.create","client_ref":"rival-question","payload":{
                "importance":"high","wording":"Could the retry timer explain the failures?"}}),
            json!({"op":"assumption.create","client_ref":"timer","payload":{
                "affected_refs":[f.conclusion],"claim":"Retries were disabled in the staged run.",
                "consequence":"Failures cannot come from retries.","materiality":"material"}}),
            op("review.submit", payload),
        ])
        .unwrap();
    // Only new records: no Conclusion, Decision or Question is changed.
    assert!(accepted.updated.is_empty());
    assert_eq!(
        case.state.current(f.conclusion.id).unwrap().reference(),
        f.conclusion
    );
    assert_eq!(case.get(f.conclusion).body["status"], "candidate");
    let rival = accepted.records[0].reference();
    let timer = accepted.records[1].reference();
    let stored = &accepted.records[2].body;
    assert_eq!(stored["audit"]["scope"], "subject");
    assert_eq!(stored["audit"]["subject_ref"], json!(f.conclusion));
    assert_eq!(stored["audit"]["reported_gap_ids"], json!([gap_id]));
    assert_eq!(stored["audit"]["undispositioned_gap_ids"], json!([]));
    let disposition = &stored["gap_dispositions"][0];
    assert_eq!(
        disposition["check_id"],
        "conclusion.causal_claim_without_rival_account"
    );
    assert_eq!(disposition["audited_subject_refs"], json!([f.conclusion]));
    assert_eq!(disposition["related_refs"], json!([f.hypothesis]));
    assert_eq!(disposition["record_refs"], json!([rival]));
    let novel = &stored["novel_gaps"];
    assert_eq!(novel.as_array().unwrap().len(), 2);
    assert_eq!(novel[0]["origin"], "reviewer");
    assert_eq!(novel[0]["record_refs"], json!([rival, timer]));
    assert_eq!(novel[0]["related_refs"], json!([f.question]));
    assert!(novel[1]["uncertainty"].is_null());
    assert!(novel[0].get("check_id").is_none());

    // The checker does not claim it should have found the reviewer's gaps:
    // the audit after the review reports the same structural gaps as before.
    let after = case.audit(Some(f.conclusion.id));
    assert_eq!(
        after
            .checks
            .iter()
            .map(|finding| &finding.gap_id)
            .collect::<Vec<_>>(),
        before
            .checks
            .iter()
            .map(|finding| &finding.gap_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_disposition_must_name_a_gap_the_reviewed_audit_reported() {
    let mut case = Case::new();
    let f = fixture(&mut case);
    let response = case.audit(None);
    let gap_id = causal_gap(&response);
    let base = review(f.conclusion, &response, json!([]));
    let submit =
        |case: &mut Case, payload: Value| case.run(vec![op("review.submit", payload)]).unwrap_err();

    let mut invented = base.clone();
    invented["gap_dispositions"] = json!([{"gap_id":"gap:invented.rule:x","disposition":"not_applicable","reason":"Out of scope."}]);
    let failure = submit(&mut case, invented);
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/gap_dispositions/0/gap_id")
    );
    assert_eq!(failure.error.expected, Some(json!([gap_id])));

    let mut unnamed = base.clone();
    unnamed.as_object_mut().unwrap().remove("audit");
    unnamed["gap_dispositions"] =
        json!([{"gap_id":gap_id,"disposition":"accepted","reason":"Real."}]);
    assert_eq!(
        submit(&mut case, unnamed)
            .error
            .path
            .as_deref()
            .map(payload_path),
        Some("/audit")
    );

    let mut digest = base.clone();
    digest["audit"]["check_set_digest"] = json!(format!("sha256:{}", "e".repeat(64)));
    let failure = submit(&mut case, digest);
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/audit/check_set_digest")
    );

    let mut snapshot = base.clone();
    snapshot["audited_snapshot"]["head_hash"] = json!(format!("sha256:{}", "d".repeat(64)));
    assert_eq!(
        submit(&mut case, snapshot)
            .error
            .path
            .as_deref()
            .map(payload_path),
        Some("/audited_snapshot")
    );

    let mut scope = base.clone();
    scope["audit"]["subject_ref"] = json!(f.hypothesis);
    assert_eq!(
        submit(&mut case, scope)
            .error
            .path
            .as_deref()
            .map(payload_path),
        Some("/audit/subject_ref")
    );

    let mut addressed = base.clone();
    addressed["gap_dispositions"] =
        json!([{"gap_id":gap_id,"disposition":"addressed","reason":"Handled."}]);
    assert_eq!(
        submit(&mut case, addressed)
            .error
            .path
            .as_deref()
            .map(payload_path),
        Some("/gap_dispositions/0/record_refs")
    );

    let mut twice = base.clone();
    twice["gap_dispositions"] = json!([
        {"gap_id":gap_id,"disposition":"accepted","reason":"Real."},
        {"gap_id":gap_id,"disposition":"not_applicable","reason":"Not."}
    ]);
    assert_eq!(
        submit(&mut case, twice)
            .error
            .path
            .as_deref()
            .map(payload_path),
        Some("/gap_dispositions/1/gap_id")
    );

    // An older audit snapshot is fine while its check set is current; a gap
    // it reported may be dispositioned even after later writes.
    case.one(
        "question.create",
        json!({"importance":"low","wording":"Unrelated?"}),
    );
    let mut older = base;
    older["gap_dispositions"] = json!([{"gap_id":gap_id,"disposition":"not_applicable","reason":"The claim is explicitly scoped to one trigger."}]);
    let accepted = case.run(vec![op("review.submit", older)]).unwrap();
    assert_eq!(
        accepted.records[0].body["audited_snapshot"],
        json!(response.snapshot)
    );
}

#[test]
fn related_records_must_be_current_and_in_the_read_set() {
    let mut case = Case::new();
    let f = fixture(&mut case);
    let response = case.audit(None);
    let mut payload = review(f.conclusion, &response, json!([]));
    payload["verdict"] = json!("gaps_identified");
    payload["novel_gaps"] =
        json!([{"statement":"A hidden constraint.","related_refs":[f.question]}]);
    let failure = case
        .run(vec![op("review.submit", payload.clone())])
        .unwrap_err();
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/read_set")
    );
    // The refusal names the records to read, not the revisions (ADR 0018).
    assert_eq!(failure.error.expected, None);
    assert_eq!(
        failure.error.actual,
        Some(json!({"records":[f.question.id]}))
    );

    payload["novel_gaps"][0]["record_refs"] = json!([f.conclusion]);
    payload["read_set"] = json!([f.question]);
    let failure = case
        .run(vec![op("review.submit", payload.clone())])
        .unwrap_err();
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/novel_gaps/0/record_refs/0")
    );

    payload["novel_gaps"][0]["record_refs"] = json!([]);
    let stale = RevisionRef {
        revision: 2,
        ..f.question
    };
    payload["novel_gaps"][0]["related_refs"] = json!([stale]);
    let failure = case.run(vec![op("review.submit", payload)]).unwrap_err();
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/novel_gaps/0/related_refs/0")
    );
}

#[test]
fn verdicts_follow_the_recorded_gaps() {
    let mut case = Case::new();
    let f = fixture(&mut case);
    let response = case.audit(None);
    let gap_id = causal_gap(&response);
    let base = review(f.conclusion, &response, json!([]));
    let path = |case: &mut Case, payload: Value| {
        case.run(vec![op("review.submit", payload)])
            .unwrap_err()
            .error
            .path
            .as_deref()
            .map(payload_path)
            .map(str::to_owned)
    };

    let mut empty = base.clone();
    empty["verdict"] = json!("gaps_identified");
    assert_eq!(path(&mut case, empty).as_deref(), Some("/gaps"));

    let mut contradicted = base.clone();
    contradicted["verdict"] = json!("no_gap_identified_within_scope");
    contradicted["no_gap_reason"] = json!("Looked.");
    contradicted["novel_gaps"] = json!([{"statement":"An unseen mechanism."}]);
    assert_eq!(path(&mut case, contradicted).as_deref(), Some("/verdict"));

    let mut accepted = base.clone();
    accepted["verdict"] = json!("no_gap_identified_within_scope");
    accepted["no_gap_reason"] = json!("Looked.");
    accepted["gap_dispositions"] =
        json!([{"gap_id":gap_id,"disposition":"accepted","reason":"Real."}]);
    assert_eq!(path(&mut case, accepted).as_deref(), Some("/verdict"));

    let mut dismissed = base.clone();
    dismissed["verdict"] = json!("no_gap_identified_within_scope");
    dismissed["no_gap_reason"] =
        json!("The audit gap is outside the stated scope and nothing else was found.");
    dismissed["gap_dispositions"] =
        json!([{"gap_id":gap_id,"disposition":"not_applicable","reason":"Scoped to one trigger."}]);
    case.run(vec![op("review.submit", dismissed)]).unwrap();

    let mut novel_only = base;
    novel_only["verdict"] = json!("gaps_identified");
    novel_only["novel_gaps"] = json!([{"statement":"An unseen mechanism."}]);
    let accepted = case.run(vec![op("review.submit", novel_only)]).unwrap();
    // The audit gap has no disposition here, which the receipt says.
    assert!(
        accepted
            .warnings
            .iter()
            .any(|warning| warning.contains(&gap_id))
    );
}

#[test]
fn no_field_can_assert_exhaustive_coverage() {
    let case = Case::new();
    let snapshot = case.snapshot();
    let mut base = json!({
        "kind":"blindspot","subject_ref":{"id":case.case.unwrap(),"revision":1},
        "audited_snapshot":snapshot,"read_set":[],"method":"Review","scope_examined":"Case",
        "checks_considered":["rivals"],"gaps":[],"no_gap_reason":"None seen in scope.",
        "evidence_refs":[],"assumption_refs":[],"limitations":["Bounded."],
        "verdict":"no_gap_identified_within_scope","context_isolation":"fresh_context_blinded",
        "context":context()
    });
    assert!(serde_json::from_value::<ReviewSubmit>(base.clone()).is_ok());
    for (field, value) in [
        ("exhaustive", json!(true)),
        ("complete", json!(true)),
        ("coverage", json!("complete")),
        ("all_blindspots_resolved", json!(true)),
    ] {
        let mut payload = base.clone();
        payload[field] = value;
        assert!(
            serde_json::from_value::<ReviewSubmit>(payload).is_err(),
            "{field}"
        );
    }
    for verdict in ["complete", "no_gaps", "verified_correct"] {
        base["verdict"] = json!(verdict);
        assert!(
            serde_json::from_value::<ReviewSubmit>(base.clone()).is_err(),
            "{verdict}"
        );
    }
    let mut gap = json!({"statement":"x","exhaustive":true});
    assert!(serde_json::from_value::<NovelGap>(gap.clone()).is_err());
    gap = json!({"gap_id":"g","disposition":"resolved_all","reason":"r"});
    assert!(serde_json::from_value::<GapDisposition>(gap).is_err());
}

#[test]
fn a_review_without_the_new_fields_keeps_its_stored_shape_and_warns_on_an_unverified_snapshot() {
    let mut case = Case::new();
    let f = fixture(&mut case);
    let mut payload = review(f.conclusion, &case.audit(None), json!([]));
    payload.as_object_mut().unwrap().remove("audit");
    payload["audited_snapshot"]["head_hash"] = json!(format!("sha256:{}", "c".repeat(64)));
    let accepted = case.run(vec![op("review.submit", payload)]).unwrap();
    let body = accepted.records[0].body.as_object().unwrap();
    for field in ["audit", "gap_dispositions", "novel_gaps"] {
        assert!(!body.contains_key(field), "{field}");
    }
    assert!(
        accepted
            .warnings
            .iter()
            .any(|warning| warning.contains("not a verified head"))
    );
}

#[test]
fn check_set_change_applies_only_to_submitted_blindspot_reviews_of_an_audit() {
    let mut case = Case::new();
    let f = fixture(&mut case);
    let response = case.audit(None);
    let reviewed = case.one("review.submit", review(f.conclusion, &response, json!([])));
    let mut record = case.get(reviewed).clone();
    assert!(!check_set_changed(&record));
    record.body["audit"]["check_set_digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    assert!(check_set_changed(&record));
    record.body["status"] = json!("superseded");
    assert!(!check_set_changed(&record));
    record.body["status"] = json!("submitted");
    record.body.as_object_mut().unwrap().remove("audit");
    assert!(!check_set_changed(&record));
}

/// A disposition may copy its finding's `related_refs` as given, even after a
/// listed record changed (bn-dem8). The current revision of each copied record
/// must then be in `read_set`. A reference the finding does not list keeps the
/// current-revision rule, and an entry may not repeat.
#[test]
fn a_disposition_accepts_its_findings_related_refs_as_given() {
    let mut case = Case::new();
    let question = case.one(
        "question.create",
        json!({"importance":"normal","wording":"Why do requests fail?"}),
    );
    let factor = case.one(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"The scheduler setting causes failures.",
            "possible_falsifiers":["Failures persist without the setting."],"rival_refs":[],"assumption_refs":[]}),
    );
    let rival = case.one(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"Client retries cause the failures.",
            "possible_falsifiers":["Failures persist without retries."],"rival_refs":[factor],"assumption_refs":[]}),
    );
    let source = case.image(Kind::Source, json!({"access": "local_only"}));
    let conclusion = case.one(
        "conclusion.create",
        json!({"statement":"The scheduler setting causes the failures.","scope":"The staged workload.",
            "significance":"Decides whether to change the setting.","support_refs":[source],"contradiction_refs":[],
            "caveats":["One workload."],"causal_factors":[{"hypothesis_ref":factor,"role":"trigger"}]}),
    );
    let before = case.audit(None);
    let finding = before
        .checks
        .iter()
        .find(|finding| finding.check_id == "conclusion.support_not_assessed_against_rival")
        .expect("the rival check reports the conclusion")
        .clone();
    assert_eq!(finding.related_refs, [rival], "{finding:?}");
    let amended = case.one(
        "hypothesis.amend",
        json!({"hypothesis_ref":rival,"rationale":"Narrow to bursts.","replacement":{
            "applicability":"The staged workload","assumption_refs":[],
            "possible_falsifiers":["Failures persist without retries."],
            "question_ref":question,"rival_refs":[factor],
            "statement":"Client retries cause the failures during bursts."}}),
    );
    assert_eq!(amended.revision, 2);

    let mut payload = review(conclusion, &before, json!([question, factor, amended]));
    payload["verdict"] = json!("gaps_identified");
    payload["gap_dispositions"] = json!([{
        "gap_id": finding.gap_id, "disposition": "accepted",
        "reason": "The support was not assessed against the retry account.",
        "related_refs": finding.related_refs, "record_refs": []
    }]);

    // The copied revision is no longer current and is still accepted;
    // without the current revision in read_set the review names it.
    let mut missing = payload.clone();
    missing["read_set"] = json!([question, factor]);
    let failure = case.run(vec![op("review.submit", missing)]).unwrap_err();
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/read_set"),
        "{failure:?}"
    );
    assert_eq!(failure.error.expected, None, "{failure:?}");
    assert!(
        failure.error.actual.as_ref().unwrap()["records"]
            .as_array()
            .unwrap()
            .contains(&json!(amended.id)),
        "{failure:?}"
    );

    // A stale reference the finding does not list keeps the current rule.
    let mut other = payload.clone();
    let stale = RevisionRef {
        revision: 9,
        ..question
    };
    other["gap_dispositions"][0]["related_refs"] = json!([rival, stale]);
    let failure = case.run(vec![op("review.submit", other)]).unwrap_err();
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/gap_dispositions/0/related_refs/1")
    );

    // An entry may not repeat.
    let mut repeated = payload.clone();
    repeated["gap_dispositions"][0]["related_refs"] = json!([rival, rival]);
    let failure = case.run(vec![op("review.submit", repeated)]).unwrap_err();
    assert_eq!(
        failure.error.path.as_deref().map(payload_path),
        Some("/gap_dispositions/0/related_refs/1")
    );

    let accepted = case.run(vec![op("review.submit", payload)]).unwrap();
    let stored = &accepted.records[0].body;
    assert_eq!(
        stored["gap_dispositions"][0]["related_refs"],
        json!([rival])
    );
    assert!(
        stored["read_set"]
            .as_array()
            .unwrap()
            .contains(&json!(amended)),
        "{stored:#}"
    );
}
