use serde_json::{Value, json};

use super::super::{AuditRequest, AuditResponse, Finding, audit, checks};
use super::{HARD_CELL_MISSING, HARD_CELL_UNKNOWN, STALL_COLLECTION_TRANSACTIONS};
use crate::{
    basis::basis_digest,
    catalog::standard_catalog,
    command::{Command, RevisionRef},
    gap_explain::gap_rule,
    gap_rules::GAP_RULE_VERSION,
    header::SystemClock,
    identity::{Id, SystemIdSource},
    review_ops::required_read_set,
    state::{Acceptance, State, validate_and_apply},
    wire::{Failure, Snapshot},
};

struct Case {
    state: State,
    case: Option<Id>,
}

/// The Scenario C decision frame before any measurement: three eligible
/// options, two hard criteria and one soft criterion.
struct Frame {
    question: RevisionRef,
    options: Vec<RevisionRef>,
    recovery: RevisionRef,
    loss: RevisionRef,
    cost: RevisionRef,
}

impl Case {
    fn new(mode: &str) -> Self {
        let mut case = Self {
            state: State::default(),
            case: None,
        };
        case.one(
            "case.create",
            json!({"title":"Queue architecture choice","mode":mode}),
        );
        case
    }

    fn try_run(&mut self, operations: Vec<Value>) -> Result<Vec<RevisionRef>, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version":"inv.command.v1","command_id":format!("evaluative-{sequence}"),
            "case_id":self.case,"actor":"agent:test","preconditions":[],"operations":operations,
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
        self.state.note_verified_snapshot(self.snapshot())?;
        self.case.get_or_insert(accepted.case_id);
        Ok(accepted
            .records
            .iter()
            .map(|record| record.reference())
            .collect())
    }

    fn one(&mut self, op: &str, payload: Value) -> RevisionRef {
        *self
            .try_run(vec![json!({"op":op,"payload":payload})])
            .unwrap_or_else(|failure| panic!("{op}: {failure:?}"))
            .last()
            .unwrap()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sequence: self.state.sequence(),
            head_hash: format!("sha256:{:064x}", self.state.sequence())
                .parse()
                .unwrap(),
        }
    }

    fn case_ref(&self) -> RevisionRef {
        self.state.current(self.case.unwrap()).unwrap().reference()
    }

    fn audit(&self) -> AuditResponse {
        audit(
            &self.state,
            &self.snapshot(),
            self.case.unwrap(),
            &AuditRequest::default(),
        )
        .unwrap()
    }

    fn findings(&self, check_id: &str) -> Vec<Finding> {
        self.audit()
            .checks
            .into_iter()
            .filter(|finding| finding.check_id == check_id)
            .collect()
    }

    fn frame(&mut self) -> Frame {
        let question = self.one(
            "question.create",
            json!({"importance":"high","wording":"Which queue design should the application use?"}),
        );
        let options = [
            "Retain database-backed queue",
            "Adopt managed queue",
            "Run dedicated broker",
        ]
        .map(|description| {
            let option = self.one(
                "option.create",
                json!({"question_ref":question,"description":description,"dependencies":[],"tradeoff_notes":[]}),
            );
            self.one(
                "option.disposition",
                json!({"option_ref":option,"lifecycle":"eligible","reason":"Plausible candidate"}),
            )
        })
        .to_vec();
        let recovery = self.one(
            "criterion.create",
            criterion(
                question,
                "Recovery time",
                "hard",
                Some("seconds"),
                json!({"kind":"measured","operator":"at_most","value":{"decimal":"300","unit":"seconds"}}),
            ),
        );
        let loss = self.one(
            "criterion.create",
            criterion(
                question,
                "No acknowledged job is lost",
                "hard",
                None,
                json!({"kind":"qualitative","requirement":"Zero acknowledged jobs lost"}),
            ),
        );
        let cost = self.one(
            "criterion.create",
            criterion(
                question,
                "Operational cost",
                "soft",
                Some("USD/month"),
                Value::Null,
            ),
        );
        Frame {
            question,
            options,
            recovery,
            loss,
            cost,
        }
    }

    fn assumption(&mut self, materiality: &str) -> RevisionRef {
        self.one(
            "assumption.create",
            json!({"claim":"Traffic stays below the projected peak.","consequence":"The cost ordering may change.",
                "materiality":materiality,"affected_refs":[]}),
        )
    }

    fn compare(&mut self, option: RevisionRef, criterion: RevisionRef, cell: Value) -> RevisionRef {
        let mut payload = json!({"option_ref":option,"criterion_ref":criterion,"method":"Recorded fixture",
            "evidence_refs":[],"assumption_refs":[],"uncertainty":"One fixture.","rationale":"Fixture cell."});
        for (key, value) in cell.as_object().unwrap() {
            payload[key] = value.clone();
        }
        self.one("comparison.add", payload)
    }

    fn unknown(&mut self, option: RevisionRef, criterion: RevisionRef) -> RevisionRef {
        self.compare(
            option,
            criterion,
            json!({"outcome":"unknown","basis":{"kind":"unknown","reason":"Not measured."}}),
        )
    }

    fn validate(&mut self, subject: RevisionRef) -> RevisionRef {
        let record = self.state.revision(subject).unwrap();
        let basis = basis_digest(record).unwrap();
        let reads = required_read_set(&self.state, record).unwrap();
        self.one(
            "review.submit",
            json!({
                "kind":"validation","subject_ref":subject,"basis_digest":basis,"read_set":reads,
                "method":"Inspect the recorded cells.","materials_examined":[],"shared_dependencies":[],
                "objection_refs":[],"verdict":"supports","caveats":["One fixture."],
                "context_isolation":"fresh_context_unblinded","context":{
                    "saw_original_conclusion":{"state":"known","value":true},
                    "gathered_new_evidence":{"state":"known","value":false},
                    "method_or_source_changes":[],"activity":"Read the recorded basis.",
                    "dependencies":"shared","dependence_basis":"Same recorded data."}
            }),
        )
    }

    fn objection(&mut self, subject: RevisionRef, severity: &str) -> RevisionRef {
        self.one(
            "objection.raise",
            json!({"subject_ref":subject,"statement":"A withheld concern.","scope":"The recorded choice.",
                "severity":severity,"evidence_refs":{"state":"unknown"}}),
        )
    }

    fn observe(&mut self, value: &str) -> RevisionRef {
        self.one(
            "observation.add",
            json!({
                "value":{"kind":"text","statement":format!("Reading {value} was reported.")},
                "basis":{"kind":"assertion","assertion":"Reported by the recorder.","context":null},
                "validity":"unknown","quality":[],"limitations":[],
                "evidence_kind":"observational","variables_changed":[]
            }),
        )
    }

    fn retrospective(&mut self) -> RevisionRef {
        let sequence = self.state.sequence();
        let payload = json!({
            "case_ref":self.case_ref(),
            "snapshot":{"sequence":sequence,"head_hash":self.state.verified_snapshot(sequence).unwrap()},
            "methods_attempted":[{"name":"Cell-by-cell comparison"}],"effective_steps":[],
            "invalid_or_misleading_steps":[],"none_observed_reason":"No invalid work was recorded.",
            "costs_or_constraints":"One fixture.","residual_unknowns":["Recovery under production load."],
            "lessons":[],"no_reusable_lesson_reason":"One fixture is not portable."
        });
        self.one("retrospective.record", payload)
    }
}

fn criterion(
    question: RevisionRef,
    name: &str,
    kind: &str,
    unit: Option<&str>,
    threshold: Value,
) -> Value {
    json!({
        "name":name,"kind":kind,"preferred_direction":"lower",
        "scale":{"unit":unit,"description":name,"anchors":[]},
        "scope":{"kind":"all_options","question_ref":question},
        "threshold":threshold,"weighting_rationale":null
    })
}

fn facts(finding: &Finding, name: &str) -> Vec<RevisionRef> {
    finding
        .facts
        .iter()
        .filter(|fact| fact.fact == name)
        .flat_map(|fact| fact.refs.iter().copied())
        .collect()
}

fn has_fact(finding: &Finding, prefix: &str) -> bool {
    finding
        .facts
        .iter()
        .any(|fact| fact.fact.starts_with(prefix))
}

fn sorted(mut refs: Vec<RevisionRef>) -> Vec<RevisionRef> {
    refs.sort_by_key(|reference| (reference.id, reference.revision));
    refs
}

#[test]
fn registered_conditions_match_their_constants() {
    let registry = checks();
    for id in [
        "criterion.hard_unmeasured",
        "comparison.unit_mismatch",
        "decision.preference_without_tradeoff",
        "promoted.material_assumption_unsupported",
        "promoted.open_material_objection",
        "case.collection_without_consequence",
        "case.material_closure_without_current_retrospective",
    ] {
        assert!(registry.iter().any(|check| check.check_id == id), "{id}");
    }
    // The hard-cell check groups these rules of this rule version; a new rule
    // version must revisit the check and bump its version.
    assert_eq!(GAP_RULE_VERSION, "slice-gaps.v1");
    assert!(gap_rule(HARD_CELL_MISSING).is_some());
    assert!(gap_rule(HARD_CELL_UNKNOWN).is_some());
    let stall = registry
        .iter()
        .find(|check| check.check_id == "case.collection_without_consequence")
        .unwrap();
    assert!(
        stall
            .condition
            .contains(&format!("{STALL_COLLECTION_TRANSACTIONS} or more")),
        "{}",
        stall.condition
    );
}

#[test]
fn scenario_c_before_the_recovery_measurement_reports_each_unmeasured_hard_criterion() {
    let mut case = Case::new("evaluate");
    let frame = case.frame();
    let findings = case.findings("criterion.hard_unmeasured");
    assert_eq!(findings.len(), 2, "{findings:#?}");
    let subjects: Vec<Vec<RevisionRef>> = findings
        .iter()
        .map(|finding| finding.subject_refs.clone())
        .collect();
    assert!(subjects.contains(&vec![frame.recovery]));
    assert!(subjects.contains(&vec![frame.loss]));
    assert!(!subjects.contains(&vec![frame.cost]));
    for finding in &findings {
        let criterion = finding.subject_refs[0];
        assert_eq!(
            finding.gap_id,
            format!("blindspot:criterion.hard_unmeasured:{}", criterion.id)
        );
        assert_eq!(finding.suggested_schema.as_deref(), Some("comparison.add"));
        assert_eq!(finding.guide.as_deref(), Some("compare"));
        assert_eq!(
            facts(finding, "hard_cell:missing"),
            sorted(frame.options.clone())
        );
        assert!(facts(finding, "hard_cell:unknown").is_empty());
        // Each cell names the frontier gap that `next` and `frontier` show.
        let frontier: Vec<_> = finding
            .facts
            .iter()
            .filter(|fact| fact.fact.starts_with("frontier_gap:gap:"))
            .collect();
        assert_eq!(frontier.len(), 3);
        let gaps =
            crate::gap_rules::enumerate_gaps(&case.state, &case.snapshot(), case.case.unwrap())
                .unwrap();
        for fact in frontier {
            let gap_id = fact.fact.strip_prefix("frontier_gap:").unwrap();
            assert!(gaps.gaps.iter().any(|gap| gap.gap_id == gap_id), "{gap_id}");
        }
    }
    // Nothing promoted, nothing decided: no other check of this family reports.
    let audit = case.audit();
    assert!(
        audit
            .checks
            .iter()
            .all(|finding| finding.check_id == "criterion.hard_unmeasured"),
        "{:#?}",
        audit.checks
    );

    // The recovery measurement for one option, unknown cells for the others,
    // and an assessed loss requirement for every option.
    let support = case.assumption("nonmaterial");
    case.compare(
        frame.options[0],
        frame.recovery,
        json!({"outcome":"meets","value":{"decimal":"110","unit":"seconds"},"assumption_refs":[support]}),
    );
    let unknown: Vec<_> = frame.options[1..]
        .iter()
        .map(|option| case.unknown(*option, frame.recovery))
        .collect();
    for option in &frame.options {
        case.compare(
            *option,
            frame.loss,
            json!({"outcome":"meets","basis":{"kind":"qualitative","statement":"Recorded design."},"assumption_refs":[support]}),
        );
    }
    let findings = case.findings("criterion.hard_unmeasured");
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].subject_refs, vec![frame.recovery]);
    assert!(facts(&findings[0], "hard_cell:missing").is_empty());
    assert_eq!(facts(&findings[0], "hard_cell:unknown"), sorted(unknown));
    assert_eq!(frame.question.revision, 1);
}

#[test]
fn a_changed_criterion_unit_is_a_gap_never_a_conversion() {
    let mut case = Case::new("evaluate");
    let frame = case.frame();
    let support = case.assumption("nonmaterial");
    let cell = case.compare(
        frame.options[0],
        frame.recovery,
        json!({"outcome":"meets","value":{"decimal":"110","unit":"seconds"},"assumption_refs":[support]}),
    );
    assert!(case.findings("comparison.unit_mismatch").is_empty());
    let amended = case.one(
        "criterion.amend",
        json!({"criterion_ref":frame.recovery,"reason":"Report in milliseconds.","replacement":criterion(
            frame.question, "Recovery time", "hard", Some("milliseconds"),
            json!({"kind":"measured","operator":"at_most","value":{"decimal":"300000","unit":"milliseconds"}}))}),
    );
    let findings = case.findings("comparison.unit_mismatch");
    assert_eq!(findings.len(), 1, "{findings:#?}");
    let finding = &findings[0];
    assert_eq!(finding.subject_refs, vec![cell]);
    assert_eq!(facts(finding, "unit_mismatch"), sorted(vec![cell, amended]));
    assert_eq!(
        facts(finding, "criterion_revised_since_comparison"),
        sorted(vec![frame.recovery, amended])
    );
    assert_eq!(
        finding.suggested_schema.as_deref(),
        Some("comparison.revise")
    );
    // The values are never quoted, converted or compared.
    // Only the finding's prose and fact names are checked: ids are random and
    // can contain any digit run, so the serialized document is not a safe
    // haystack.
    let mut prose = vec![finding.severity_reason.as_str(), finding.why.as_str()];
    prose.extend(finding.facts.iter().map(|fact| fact.fact.as_str()));
    prose.extend(finding.guide.as_deref());
    for text in prose {
        assert!(
            !text.contains("110") && !text.contains("milliseconds"),
            "{text}"
        );
    }
}

#[test]
fn decisions_show_preferences_unsupported_assumptions_and_later_material_objections() {
    let mut case = Case::new("evaluate");
    let frame = case.frame();
    let support = case.assumption("nonmaterial");
    let traffic = case.assumption("material");
    let recovery = case.compare(
        frame.options[0],
        frame.recovery,
        json!({"outcome":"meets","value":{"decimal":"110","unit":"seconds"},"assumption_refs":[support]}),
    );
    let loss = case.unknown(frame.options[0], frame.loss);
    let cost = case.compare(
        frame.options[0],
        frame.cost,
        json!({"outcome":"meets","basis":{"kind":"preference","statement":"The team prefers it."},"assumption_refs":[support]}),
    );
    let proposed = case.one(
        "decision.propose",
        json!({
            "question_ref":frame.question,"choice":{"kind":"select","option_ref":frame.options[0]},
            "compared_alternatives":frame.options,"comparison_refs":[recovery,loss,cost],
            "hard_conditions":[{"criterion_ref":frame.recovery,"reason":"Verify independently."},
                {"criterion_ref":frame.loss,"reason":"Verify independently."}],
            "evidence_refs":[],"assumption_refs":[traffic],"tradeoffs":[],
            "consequences":["Retain the existing queue."],"reopen_trigger":"Traffic exceeds the projection.",
            "reversal_conditions":[]
        }),
    );
    let preference = case.findings("decision.preference_without_tradeoff");
    assert_eq!(preference.len(), 1, "{preference:#?}");
    assert_eq!(preference[0].subject_refs, vec![proposed]);
    assert_eq!(
        facts(&preference[0], "comparison_basis:preference"),
        vec![cost]
    );
    // Not promoted yet: the assumption does not bear on reliance.
    assert!(
        case.findings("promoted.material_assumption_unsupported")
            .is_empty()
    );

    // A material objection known before review is accounted for at acceptance.
    let earlier = case.objection(proposed, "material");
    let review = case.validate(proposed);
    let accepted = case.one(
        "decision.accept",
        json!({"decision_ref":proposed,"review_ref":review}),
    );
    let audit = case.audit();
    let assumption: Vec<_> = audit
        .checks
        .iter()
        .filter(|finding| finding.check_id == "promoted.material_assumption_unsupported")
        .collect();
    assert_eq!(assumption.len(), 1, "{:#?}", audit.checks);
    assert_eq!(assumption[0].subject_refs, vec![accepted]);
    assert_eq!(
        facts(assumption[0], "assumption_status:unassessed"),
        vec![traffic]
    );
    assert_eq!(
        assumption[0].suggested_schema.as_deref(),
        Some("assumption.assess")
    );
    assert!(
        !audit
            .checks
            .iter()
            .any(|finding| finding.check_id == "promoted.open_material_objection"),
        "{:#?}",
        audit.checks
    );
    // The preference finding follows the accepted revision.
    assert_eq!(
        case.findings("decision.preference_without_tradeoff")[0].subject_refs,
        vec![accepted]
    );

    // A material objection raised after acceptance is new to the promotion.
    let later = case.objection(accepted, "material");
    let objection = case.findings("promoted.open_material_objection");
    assert_eq!(objection.len(), 1, "{objection:#?}");
    assert_eq!(objection[0].subject_refs, vec![accepted]);
    assert_eq!(facts(&objection[0], "open_objection:material"), vec![later]);
    assert!(!facts(&objection[0], "open_objection:material").contains(&earlier));
    assert_eq!(
        facts(
            &objection[0],
            &format!(
                "promoted_at_transaction:{}",
                case.state
                    .revision(accepted)
                    .unwrap()
                    .header
                    .acceptance_sequence
            )
        ),
        vec![accepted]
    );
    // A critical objection stays with the seed check alone.
    case.objection(accepted, "critical");
    let audit = case.audit();
    let material = audit
        .checks
        .iter()
        .find(|finding| finding.check_id == "promoted.open_material_objection")
        .unwrap();
    assert_eq!(facts(material, "open_objection:material"), vec![later]);
    assert!(
        audit
            .checks
            .iter()
            .any(|finding| finding.check_id == "promoted.open_critical_objection")
    );

    // A challenged assumption is still unsupported; a supported one is not.
    let challenged = case.one(
        "assumption.assess",
        json!({"assumption_ref":traffic,"status":"challenged","rationale":"Peak traffic is disputed.","evidence_refs":[]}),
    );
    let findings = case.findings("promoted.material_assumption_unsupported");
    assert_eq!(
        facts(&findings[0], "assumption_status:challenged"),
        vec![challenged]
    );
    case.one(
        "assumption.assess",
        json!({"assumption_ref":challenged,"status":"supported","rationale":"Measured peak traffic.","evidence_refs":[]}),
    );
    assert!(
        case.findings("promoted.material_assumption_unsupported")
            .is_empty()
    );
}

#[test]
fn stalled_collection_cites_the_transactions_it_counted() {
    let mut case = Case::new("record");
    let mut collected = Vec::new();
    for value in 1..STALL_COLLECTION_TRANSACTIONS {
        collected.push(case.observe(&value.to_string()));
    }
    assert!(
        case.findings("case.collection_without_consequence")
            .is_empty()
    );
    collected.push(case.observe("4"));
    let findings = case.findings("case.collection_without_consequence");
    assert_eq!(findings.len(), 1, "{findings:#?}");
    let finding = &findings[0];
    assert_eq!(finding.subject_refs, vec![case.case_ref()]);
    assert_eq!(finding.guide.as_deref(), Some("stalled"));
    assert!(has_fact(finding, "last_consequence_transaction:none"));
    assert!(has_fact(
        finding,
        &format!("collection_transactions_counted:{STALL_COLLECTION_TRANSACTIONS}")
    ));
    for observation in &collected {
        let sequence = case
            .state
            .revision(*observation)
            .unwrap()
            .header
            .acceptance_sequence;
        assert_eq!(
            facts(finding, &format!("collection_transaction:{sequence}")),
            vec![*observation]
        );
    }

    // An inconclusive assessment has no discriminatory effect.
    let question = case.one(
        "question.create",
        json!({"importance":"normal","wording":"Why is it slow?"}),
    );
    let hypothesis = case.one(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"The cache is cold.","possible_falsifiers":["Warm runs are as slow."],"rival_refs":[],"assumption_refs":[]}),
    );
    let assess = |case: &mut Case, evidence: RevisionRef, relation: &str| {
        case.one(
            "assessment.add",
            json!({"subject_ref":hypothesis,"evidence_refs":[evidence],"relation":relation,"rationale":"Fixture."}),
        )
    };
    assess(&mut case, collected[0], "inconclusive");
    assert_eq!(
        case.findings("case.collection_without_consequence").len(),
        1
    );
    // Activating the hypothesis is not a consequence of evidence either.
    let active = case.one(
        "hypothesis.disposition",
        json!({"hypothesis_ref":hypothesis,"lifecycle":"active","rationale":"Worth testing.","evidence_refs":[]}),
    );
    assert_eq!(
        case.findings("case.collection_without_consequence").len(),
        1
    );
    let retired = case.one(
        "hypothesis.disposition",
        json!({"hypothesis_ref":active,"lifecycle":"retired","rationale":"No longer useful.","evidence_refs":[]}),
    );
    assert!(
        case.findings("case.collection_without_consequence")
            .is_empty()
    );

    // Collection resumes; the count restarts after the consequence.
    for value in 0..STALL_COLLECTION_TRANSACTIONS {
        case.observe(&format!("1{value}"));
    }
    let findings = case.findings("case.collection_without_consequence");
    let sequence = case
        .state
        .revision(retired)
        .unwrap()
        .header
        .acceptance_sequence;
    assert_eq!(
        facts(
            &findings[0],
            &format!("last_consequence_transaction:{sequence}")
        ),
        vec![retired]
    );
    assert!(!has_fact(
        &findings[0],
        &format!(
            "collection_transaction:{}",
            case.state
                .revision(collected[0])
                .unwrap()
                .header
                .acceptance_sequence
        )
    ));
}

#[test]
fn a_decided_material_case_needs_a_current_retrospective_before_closure() {
    let mut case = Case::new("evaluate");
    let frame = case.frame();
    let check = "case.material_closure_without_current_retrospective";
    let cells: Vec<_> = [frame.recovery, frame.loss]
        .into_iter()
        .map(|criterion| case.unknown(frame.options[0], criterion))
        .collect();
    let proposed = case.one(
        "decision.propose",
        json!({
            "question_ref":frame.question,"choice":{"kind":"select","option_ref":frame.options[0]},
            "compared_alternatives":frame.options,"comparison_refs":cells,
            "hard_conditions":[{"criterion_ref":frame.recovery,"reason":"Measure it."},
                {"criterion_ref":frame.loss,"reason":"Verify it."}],
            "evidence_refs":[],"assumption_refs":[],"tradeoffs":["Unmeasured recovery."],
            "consequences":["Retain the existing queue."],"reopen_trigger":"A recovery measurement.",
            "reversal_conditions":[]
        }),
    );
    // Proposed is not decided, and nothing material has happened yet.
    assert!(case.findings(check).is_empty());
    let review = case.validate(proposed);
    let accepted = case.one(
        "decision.accept",
        json!({"decision_ref":proposed,"review_ref":review}),
    );
    let findings = case.findings(check);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].subject_refs, vec![case.case_ref()]);
    assert_eq!(
        facts(&findings[0], "closure_signal:all_questions_decided"),
        vec![accepted]
    );
    assert!(facts(&findings[0], "material_work").contains(&accepted));
    assert!(has_fact(&findings[0], "retrospective:none"));
    assert_eq!(
        findings[0].suggested_schema.as_deref(),
        Some("retrospective.record")
    );

    let retrospective = case.retrospective();
    assert!(case.findings(check).is_empty());

    // Work after the account makes it out of date.
    let objection = case.objection(accepted, "material");
    let findings = case.findings(check);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(
        facts(&findings[0], "retrospective:not_current"),
        vec![retrospective]
    );
    assert!(facts(&findings[0], "changed_after_retrospective").contains(&objection));
}
