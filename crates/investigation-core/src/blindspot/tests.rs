use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    gap_explain::gap_rule,
    header::SystemClock,
    identity::SystemIdSource,
    method_pack::builtin_general,
    state::{Acceptance, validate_and_apply},
};

/// Words a clean or dirty audit must never use about itself.
const FORBIDDEN: &[&str] = &[
    "all blindspots resolved",
    "verified correct",
    "investigation complete",
    "no blindspots",
];

fn assert_no_completeness_claim(text: &str) {
    let lower = text.to_ascii_lowercase();
    for phrase in FORBIDDEN {
        assert!(!lower.contains(phrase), "{phrase:?} in {text}");
    }
}

#[test]
fn every_registered_check_is_well_formed_and_unique() {
    let catalog = standard_catalog();
    let general = builtin_general();
    let registry = checks();
    assert!(!registry.is_empty());
    let mut seen = BTreeSet::new();
    let mut exact_rules = BTreeSet::new();
    let mut prefixes = Vec::new();
    for check in &registry {
        assert!(seen.insert(check.check_id), "duplicate {}", check.check_id);
        let id = check.check_id;
        assert!(
            id.split('.').count() >= 2
                && id.split('.').all(|part| !part.is_empty()
                    && part.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_')),
            "{id} is not a dotted lowercase check ID"
        );
        assert!(
            check.version.starts_with('v') && check.version.len() > 1,
            "{id}"
        );
        assert!(!check.subject_kinds.is_empty(), "{id}");
        assert!(!check.reads.is_empty(), "{id}");
        for text in [check.condition, check.why, check.severity_reason] {
            assert!(!text.trim().is_empty(), "{id}");
            assert_no_completeness_claim(text);
        }
        if let Some(schema) = check.suggested_schema {
            assert!(
                catalog.available_operation(schema).is_ok(),
                "{id} suggests unknown operation {schema}"
            );
        }
        if let Some(card) = check.guide {
            assert!(
                general.card(card).is_some(),
                "{id} names unknown card {card}"
            );
        }
        if let CheckSource::GapRules { rules, .. } = check.source {
            match rules {
                GapRuleMatch::Exact(ids) => {
                    for rule in ids {
                        assert!(
                            gap_rule(rule).is_some(),
                            "{id} adopts unknown gap rule {rule}"
                        );
                        assert!(exact_rules.insert(*rule), "gap rule {rule} adopted twice");
                    }
                }
                GapRuleMatch::Prefix(prefix) => prefixes.push(prefix),
            }
        }
    }
    // A gap is reported by at most one check.
    for rule in &exact_rules {
        assert!(
            !prefixes.iter().any(|prefix| rule.starts_with(prefix)),
            "{rule}"
        );
    }
    let ids: Vec<_> = registry.iter().map(|check| check.check_id).collect();
    let mut ordered = ids.clone();
    ordered.sort_unstable();
    assert_eq!(ids, ordered);
}

#[test]
fn the_check_set_digest_changes_with_any_check_version() {
    let registry = checks();
    let digest = check_set_digest(&registry);
    assert_eq!(digest, check_set_digest(&registry));
    let mut changed = *registry[0];
    changed.version = "v999";
    let mut altered = registry.clone();
    altered[0] = &changed;
    assert_ne!(digest, check_set_digest(&altered));
    assert_ne!(digest, check_set_digest(&registry[1..]));
}

struct Case {
    state: State,
    case: Option<Id>,
}

impl Case {
    fn new() -> Self {
        let mut test = Self {
            state: State::default(),
            case: None,
        };
        test.submit(
            "case.create",
            json!({"title": "Blindspot audit", "mode": "record"}),
        );
        test
    }

    fn submit(&mut self, op: &str, payload: Value) -> RevisionRef {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("command-{sequence}"),
            "case_id": self.case,
            "actor": "agent:test",
            "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
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
        )
        .unwrap();
        self.state.apply(&accepted).unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        self.case.get_or_insert(accepted.case_id);
        accepted.records.last().unwrap().reference()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sequence: self.state.sequence(),
            head_hash: format!("sha256:{:064x}", self.state.sequence())
                .parse()
                .unwrap(),
        }
    }

    fn audit(&self, subject: Option<Id>) -> Result<AuditResponse, Failure> {
        audit(
            &self.state,
            &self.snapshot(),
            self.case.unwrap(),
            &AuditRequest { subject },
        )
    }
}

fn validate(response: &AuditResponse) {
    let schema = serde_json::to_value(audit_schema()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let instance = serde_json::to_value(response).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}\n{instance:#}");
}

#[test]
fn a_clean_case_reports_no_gap_under_the_named_checks_and_no_completeness() {
    let case = Case::new();
    let response = case.audit(None).unwrap();
    validate(&response);
    assert_eq!(
        response.outcome,
        AuditOutcome::NoStructuralGapIdentifiedUnderChecks
    );
    assert!(response.checks.is_empty());
    assert_eq!(response.checks_run.len(), checks().len());
    assert!(
        response
            .statement
            .starts_with("No structural gap identified under checks "),
        "{}",
        response.statement
    );
    for check in checks() {
        assert!(response.statement.contains(check.check_id));
    }
    assert!(response.limits.contains("not proof of completeness"));
    let text = serde_json::to_string(&response).unwrap();
    assert_no_completeness_claim(&text);
    assert_eq!(response, case.audit(None).unwrap());
}

#[test]
fn identical_snapshots_give_identical_audits_with_stable_gap_ids() {
    let mut case = Case::new();
    let question = case.submit(
        "question.create",
        json!({"importance":"normal","wording":"Why did it happen?"}),
    );
    let hypothesis = case.submit(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"The setting caused it.","possible_falsifiers":["The effect persists without the setting."],"rival_refs":[],"assumption_refs":[]}),
    );
    let first = case.audit(None).unwrap();
    validate(&first);
    let again = case.audit(None).unwrap();
    assert_eq!(first, again);
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&again).unwrap()
    );
    // A Hypothesis with no assessment is a frontier gap but not one of these
    // checks: it must not appear, and the statement stays bounded.
    assert!(
        first
            .checks
            .iter()
            .all(|finding| !finding.subject_refs.iter().any(|r| r.id == hypothesis.id))
    );
}

#[test]
fn subject_narrowing_accepts_only_conclusions_comparisons_and_decisions() {
    let mut case = Case::new();
    let question = case.submit(
        "question.create",
        json!({"importance":"normal","wording":"Which one?"}),
    );
    let failure = case.audit(Some(question.id)).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert_eq!(failure.error.path.as_deref(), Some("--subject"));
    let absent: Id = "concl_019acd16-0000-7000-8000-0000000000ff"
        .parse()
        .unwrap();
    let failure = case.audit(Some(absent)).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::UnknownReference);
}
