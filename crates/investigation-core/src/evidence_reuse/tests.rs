use serde_json::{Value, json};

use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    header::SystemClock,
    identity::{IdSource, SystemIdSource},
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
    wire::RecordChange,
};

const DIGEST_T: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const DIGEST_D: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const DIGEST_H: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const DIGEST_N: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
const DIGEST_R: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";
const FROZEN_STOP: &str = "Compare after ten complete traces.";

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
        let accepted = test
            .submit(
                "case.create",
                json!({"title": "Trace study", "mode": "record"}),
            )
            .unwrap();
        test.case = Some(accepted.case_id);
        test
    }

    fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v0-alpha",
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
        )?;
        self.state.apply(&accepted)?;
        Ok(accepted)
    }

    fn create(&mut self, op: &str, payload: Value) -> RevisionRef {
        self.submit(op, payload)
            .unwrap()
            .records
            .last()
            .unwrap()
            .reference()
    }

    /// Store a record image directly, for kinds whose write path needs the
    /// blob store (Sources, Artifacts, Observations).
    fn image(&mut self, kind: Kind, body: Value, replaces: Option<RevisionRef>) -> RevisionRef {
        let case = self.case.unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = self
            .state
            .current(replaces.map_or(case, |reference| reference.id))
            .unwrap()
            .header
            .clone();
        header.acceptance_sequence = sequence;
        if replaces.is_some() {
            header.history.push(header.revision);
            header.revision.0 += 1;
        } else {
            header.id = SystemIdSource.next_id(kind).unwrap();
            header.revision.0 = 1;
            header.history.clear();
        }
        let record = Record { header, body };
        let reference = record.reference();
        let change = RecordChange {
            client_ref: None,
            id: reference.id,
            revision: reference.revision,
        };
        let (created, updated) = if replaces.is_some() {
            (vec![], vec![change])
        } else {
            (vec![change], vec![])
        };
        self.state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records: vec![record],
                created,
                updated,
                warnings: vec![],
            })
            .unwrap();
        reference
    }

    fn source(&mut self) -> RevisionRef {
        self.image(Kind::Source, json!({"access": "local_only"}), None)
    }

    fn original(&mut self, source: RevisionRef, digest: &str) -> RevisionRef {
        self.image(
            Kind::Artifact,
            json!({
                "digest": digest, "size_bytes": 24, "format": "text/plain", "source": source,
                "transforms": [], "availability": "retained", "integrity": "verified",
                "sensitivity": "internal", "role": "original",
            }),
            None,
        )
    }

    /// A renamed copy of `input`: the same captured bytes under another name.
    fn renamed(&mut self, input: RevisionRef, source: RevisionRef, digest: &str) -> RevisionRef {
        self.image(
            Kind::Artifact,
            json!({
                "digest": digest, "size_bytes": 24, "format": "text/plain", "source": source,
                "transforms": [{"input": input, "transformation": "rename", "version": "1"}],
                "availability": "retained", "integrity": "verified",
                "sensitivity": "internal", "role": "derivative",
            }),
            None,
        )
    }

    fn observe(
        &mut self,
        artifact: RevisionRef,
        digest: &str,
        run: Option<RevisionRef>,
    ) -> RevisionRef {
        self.image(
            Kind::Observation,
            json!({
                "status": "active",
                "value": {"kind": "text", "statement": "Stage three dominates the trace."},
                "basis": {"kind": "source", "artifact_ref": artifact,
                    "selector": {"schema_version": "inv.selector.v1", "artifact_digest": digest,
                        "selector": {"type": "whole"}, "validation": "verified"},
                    "text_quote": {"state": "not_applicable"}},
                "validity": "valid", "quality": [], "limitations": [], "variables_changed": [],
                "run_ref": run,
            }),
            None,
        )
    }
}

struct Study {
    case: Case,
    subject: RevisionRef,
    rival: RevisionRef,
    source: RevisionRef,
    trace: RevisionRef,
    inspected: RevisionRef,
    held_out: RevisionRef,
    plan: RevisionRef,
}

/// A trace inspected before the plan, a separately collected held-out trace
/// set nobody read, and a frozen confirmatory plan with discriminating
/// predictions.
fn study(prior_exposure: &str) -> Study {
    let mut case = Case::new();
    let question = case.create(
        "question.create",
        json!({"wording": "Why is the stage slow?", "importance": "normal"}),
    );
    let subject = case.create(
        "hypothesis.create",
        json!({"question_ref": question, "statement": "The local queue grows.",
            "possible_falsifiers": [], "rival_refs": [], "assumption_refs": []}),
    );
    let rival = case.create(
        "hypothesis.create",
        json!({"question_ref": question, "statement": "The downstream queue grows.",
            "possible_falsifiers": [], "rival_refs": [subject], "assumption_refs": []}),
    );
    let source = case.source();
    let trace = case.original(source, DIGEST_T);
    let inspected = case.observe(trace, DIGEST_T, None);
    let held_source = case.source();
    let held_out = case.original(held_source, DIGEST_H);
    let draft = case.create(
        "inquiry.create",
        json!({
            "purpose": "Discriminate the two queue explanations.",
            "method": "Compare stage timings across traces.",
            "controls": ["Same synthetic load."],
            "limits": ["Ten traces."],
            "desired_observations": ["Stage timings per trace."],
            "inquiry": {"mode": "confirmatory", "confirmatory": {
                "predictions": [
                    {"rival_ref": subject, "predicted_outcome": "Local stage time rises."},
                    {"rival_ref": rival, "predicted_outcome": "Downstream stage time rises."},
                ],
                "evidence_holdout_boundary": "Traces 11 through 20 are held out.",
                "prior_exposure": prior_exposure,
                "stop_analysis_rule": FROZEN_STOP,
            }},
        }),
    );
    let plan = case.create(
        "inquiry.freeze",
        json!({"inquiry_ref": draft, "question_ref": question}),
    );
    Study {
        case,
        subject,
        rival,
        source,
        trace,
        inspected,
        held_out,
        plan,
    }
}

impl Study {
    fn assessment(&self, evidence: RevisionRef, frozen_basis: Value) -> Value {
        json!({
            "subject_ref": self.subject, "evidence_refs": [evidence], "relation": "supports",
            "rival_expectations": [{"rival_ref": self.rival, "expectation": "less_expected",
                "rationale": "The downstream stage stayed flat."}],
            "rationale": "Local stage time rose as predicted.",
            "frozen_basis": frozen_basis,
        })
    }

    fn basis(&self, inputs: Value) -> Value {
        json!({"plan_ref": self.plan, "actual_inputs": inputs, "stop_rule_applied": FROZEN_STOP})
    }

    fn assess(
        &mut self,
        evidence: RevisionRef,
        frozen_basis: Value,
    ) -> (EvidenceReuse, Vec<String>) {
        let payload = self.assessment(evidence, frozen_basis);
        let accepted = self.case.submit("assessment.add", payload).unwrap();
        let record = accepted.records.last().unwrap();
        (
            evidence_reuse(&self.case.state, record).unwrap(),
            accepted.warnings,
        )
    }
}

#[test]
fn executor_bound_reanalysis_of_an_inspected_trace_keeps_chronology_and_reuse() {
    let mut study = study("unseen_declared");
    // After the freeze, the inspected trace is renamed and re-extracted under
    // a Run bound to the frozen plan, with an executor receipt attached.
    let renamed = study.case.renamed(study.trace, study.source, DIGEST_D);
    let receipt_source = study.case.source();
    let receipt = study.case.original(receipt_source, DIGEST_R);
    let run = study.case.create(
        "run.record",
        json!({
            "intent": {"kind": "plan", "plan_ref": study.plan},
            "producer": "agent:executor", "artifact_refs": [renamed],
            "execution_status": "completed", "evidence_completeness": "complete",
            "evidence_kind": "observational", "variables_changed": [],
            "ordering_receipt": {"artifact_ref": receipt, "format": "in_toto_dsse"},
        }),
    );
    let reextracted = study.case.observe(renamed, DIGEST_D, Some(run));
    let basis = study.basis(json!([{"input_ref": reextracted, "partition": "Trace 3"}]));
    let (reuse, warnings) = study.assess(reextracted, basis);

    assert_eq!(reuse.classification, ReuseClass::KnownOutcomeReuse);
    assert!(!reuse.counts_as_prospective);
    let input = &reuse.inputs[0];
    assert_eq!(input.input_ref, reextracted);
    assert_eq!(input.listed_as, InputListing::Both);
    assert_eq!(input.reason, InputReason::ReadBeforeTest);
    assert_eq!(input.origins, vec![study.trace]);
    assert_eq!(input.prior_exposures.len(), 1);
    let exposure = &input.prior_exposures[0];
    assert_eq!(exposure.kind, ExposureKind::ObservedBeforeFreeze);
    assert_eq!(exposure.record_ref, study.inspected);
    assert_eq!(exposure.via, study.trace.id);

    // Chronology is its own axis, read verbatim from the bound Run.
    let runs = &reuse.chronology.runs;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_ref, run);
    assert!(runs[0].binds_assessed_plan);
    assert_eq!(runs[0].chronology, "recorded_before_run_declaration");
    assert_eq!(runs[0].ordering_receipt["format"], "in_toto_dsse");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("assessment.evidence_reuse known_outcome_reuse"))
    );

    // When a verified executor receipt upgrades the Run's chronology (W07.10),
    // the strong chronology shows unchanged beside the unchanged reuse class.
    let mut upgraded = study.case.state.current(run.id).unwrap().body.clone();
    upgraded["chronology"] = json!("executor_bound");
    study.case.image(Kind::Run, upgraded, Some(run));
    let assessment = study
        .case
        .state
        .records()
        .find(|record| record.header.id.kind() == Kind::Assessment)
        .unwrap()
        .clone();
    let reread = evidence_reuse(&study.case.state, &assessment).unwrap();
    assert_eq!(reread.chronology.runs[0].chronology, "executor_bound");
    assert_eq!(reread.classification, ReuseClass::KnownOutcomeReuse);
    assert_eq!(reread.inputs, reuse.inputs);
}

#[test]
fn a_separately_held_out_acquisition_stays_distinguishable() {
    let mut study = study("unseen_declared");
    let held = study.case.observe(study.held_out, DIGEST_H, None);
    let reserved = study.basis(json!([{
        "input_ref": held, "partition": "Traces 11 through 20", "holdout": "within_reserved",
    }]));
    let (reuse, warnings) = study.assess(held, reserved);
    assert_eq!(
        reuse.classification,
        ReuseClass::HeldOutWithinDeclaredLimits
    );
    assert!(reuse.counts_as_prospective);
    assert_eq!(
        reuse.inputs[0].reason,
        InputReason::DeclaredWithinReservedHoldout
    );
    assert!(reuse.inputs[0].prior_exposures.is_empty());
    assert!(reuse.chronology.runs.is_empty());
    assert!(
        warnings
            .iter()
            .all(|warning| !warning.contains("evidence_reuse"))
    );

    // The same collected data without a holdout declaration cannot be shown
    // as held out.
    let undeclared = study.basis(json!([{"input_ref": held}]));
    let (reuse, warnings) = study.assess(held, undeclared);
    assert_eq!(reuse.classification, ReuseClass::Undetermined);
    assert_eq!(
        reuse.inputs[0].reason,
        InputReason::CollectedBeforeFreezeNotReserved
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("undetermined"))
    );

    // An acquisition recorded after the freeze is fresh.
    let fresh_source = study.case.source();
    let fresh = study.case.original(fresh_source, DIGEST_N);
    let new = study.case.observe(fresh, DIGEST_N, None);
    let (reuse, _) = study.assess(new, study.basis(json!([{"input_ref": new}])));
    assert_eq!(reuse.classification, ReuseClass::Fresh);
    assert_eq!(reuse.inputs[0].reason, InputReason::AcquiredAfterFreeze);

    // Mixing the already-inspected trace into the inputs makes the test reuse.
    let (reuse, _) = study.assess(
        new,
        study.basis(json!([{"input_ref": new}, {"input_ref": study.inspected}])),
    );
    assert_eq!(reuse.classification, ReuseClass::KnownOutcomeReuse);
    assert_eq!(reuse.inputs[0].class, ReuseClass::Fresh);
    assert_eq!(reuse.inputs[1].class, ReuseClass::KnownOutcomeReuse);
}

#[test]
fn a_look_at_reserved_data_outside_the_test_counts_as_exposure() {
    let mut study = study("unseen_declared");
    let held = study.case.observe(study.held_out, DIGEST_H, None);
    let peek = study.case.create(
        "assessment.add",
        json!({"subject_ref": study.subject, "evidence_refs": [held], "relation": "non_diagnostic",
            "rationale": "Looked at the reserved traces early."}),
    );
    let reserved = study.basis(json!([{"input_ref": held, "holdout": "within_reserved"}]));
    let (reuse, _) = study.assess(held, reserved);
    assert_eq!(reuse.classification, ReuseClass::KnownOutcomeReuse);
    let exposure = &reuse.inputs[0].prior_exposures[0];
    assert_eq!(exposure.kind, ExposureKind::AssessedOutsideTest);
    assert_eq!(exposure.record_ref, peek);

    // An earlier assessment of the same plan is part of the test, not exposure.
    let mut fresh = self::study("unseen_declared");
    let held = fresh.case.observe(fresh.held_out, DIGEST_H, None);
    let basis = fresh.basis(json!([{"input_ref": held, "holdout": "within_reserved"}]));
    fresh.assess(held, basis.clone());
    let (reuse, _) = fresh.assess(held, basis);
    assert_eq!(
        reuse.classification,
        ReuseClass::HeldOutWithinDeclaredLimits
    );
}

#[test]
fn declared_prior_exposure_limits_the_label_without_rejecting() {
    let mut seen = study("seen");
    let held = seen.case.observe(seen.held_out, DIGEST_H, None);
    let basis = seen.basis(json!([{"input_ref": held, "holdout": "within_reserved"}]));
    let (reuse, _) = seen.assess(held, basis);
    assert_eq!(reuse.classification, ReuseClass::KnownOutcomeReuse);
    assert_eq!(reuse.plan_reasons, vec![PlanReason::DeclaredSeen]);
    assert_eq!(
        reuse.inputs[0].class,
        ReuseClass::HeldOutWithinDeclaredLimits
    );
    assert_eq!(reuse.declared_prior_exposure, "seen");

    let mut unknown = study("unknown");
    let held = unknown.case.observe(unknown.held_out, DIGEST_H, None);
    let basis = unknown.basis(json!([{"input_ref": held, "holdout": "within_reserved"}]));
    let (reuse, _) = unknown.assess(held, basis);
    assert_eq!(reuse.classification, ReuseClass::Undetermined);
    assert_eq!(
        reuse.plan_reasons,
        vec![PlanReason::DeclaredExposureUnknown]
    );
}

#[test]
fn an_early_stop_is_recorded_as_a_deviation() {
    let mut study = study("unseen_declared");
    let held = study.case.observe(study.held_out, DIGEST_H, None);
    let mut basis = study.basis(json!([{"input_ref": held, "holdout": "within_reserved"}]));
    basis["stop_rule_applied"] = json!("Stopped after six of ten traces.");
    basis["exclusions"] = json!([{"excluded": "Trace 17", "reason": "The capture was truncated."}]);
    basis["deviations"] = json!([{
        "kind": "early_stop",
        "description": "Analysis stopped after six of the ten planned traces.",
        "reason": "The interim look was favorable.",
    }]);
    let (reuse, warnings) = study.assess(held, basis.clone());
    assert!(reuse.early_stop);
    assert!(!reuse.stop_rule_differs_without_deviation);
    assert_eq!(reuse.deviations[0].kind, DeviationKind::EarlyStop);
    assert_eq!(reuse.exclusions[0].excluded, "Trace 17");
    assert_eq!(reuse.frozen_stop_rule, FROZEN_STOP);
    assert_eq!(reuse.stop_rule_applied, "Stopped after six of ten traces.");
    // The deviation limits nothing on its own: the reuse axis is unchanged.
    assert_eq!(
        reuse.classification,
        ReuseClass::HeldOutWithinDeclaredLimits
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("assessment.deviation early_stop"))
    );
    let view = serde_json::to_value(&reuse).unwrap();
    assert_eq!(view["deviations"][0]["kind"], "early_stop");
    assert_eq!(view["early_stop"], true);

    // A changed stop rule with no stop deviation is reported, never blocked.
    basis["deviations"] = json!([]);
    let (reuse, warnings) = study.assess(held, basis);
    assert!(reuse.stop_rule_differs_without_deviation);
    assert!(!reuse.early_stop);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("assessment.stop_rule_differs_without_deviation"))
    );
}

#[test]
fn frozen_basis_structure_is_validated() {
    let mut study = study("unseen_declared");
    let held = study.case.observe(study.held_out, DIGEST_H, None);
    let path = |failure: Failure| failure.error.path.unwrap_or_default();

    let empty = study.basis(json!([]));
    let failure = study
        .case
        .submit("assessment.add", study.assessment(held, empty))
        .unwrap_err();
    assert_eq!(
        path(failure),
        "/operations/0/payload/frozen_basis/actual_inputs"
    );

    let mut blank = study.basis(json!([{"input_ref": held}]));
    blank["deviations"] =
        json!([{"kind": "other", "description": "Changed metric.", "reason": " "}]);
    let failure = study
        .case
        .submit("assessment.add", study.assessment(held, blank))
        .unwrap_err();
    assert_eq!(
        path(failure),
        "/operations/0/payload/frozen_basis/deviations/0/reason"
    );

    let mut overlap = study.basis(json!([{"input_ref": held}]));
    overlap["exclusions"] =
        json!([{"excluded": "The same trace.", "input_ref": held, "reason": "Noise."}]);
    let failure = study
        .case
        .submit("assessment.add", study.assessment(held, overlap))
        .unwrap_err();
    assert_eq!(
        path(failure),
        "/operations/0/payload/frozen_basis/exclusions/0/input_ref"
    );

    // Only a frozen confirmatory plan can be a frozen basis.
    let draft = study.case.create(
        "inquiry.create",
        json!({"purpose": "Look around.", "method": "Read traces.", "controls": [],
            "limits": ["Five traces."], "desired_observations": ["Timings."],
            "inquiry": {"mode": "exploratory"}}),
    );
    let mut exploratory = study.basis(json!([{"input_ref": held}]));
    exploratory["plan_ref"] = json!(draft);
    let failure = study
        .case
        .submit(
            "assessment.add",
            study.assessment(held, exploratory.clone()),
        )
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    let frozen = study
        .case
        .create("inquiry.freeze", json!({"inquiry_ref": draft}));
    exploratory["plan_ref"] = json!(frozen);
    let failure = study
        .case
        .submit("assessment.add", study.assessment(held, exploratory))
        .unwrap_err();
    assert!(failure.error.message.contains("confirmatory"));

    // An assessment without a frozen basis is unchanged and has no reuse view.
    let accepted = study
        .case
        .submit(
            "assessment.add",
            json!({"subject_ref": study.subject, "evidence_refs": [held], "relation": "non_diagnostic",
                "rationale": "Context only."}),
        )
        .unwrap();
    let record = accepted.records.last().unwrap();
    assert!(record.body.get("frozen_basis").is_none());
    assert!(evidence_reuse(&study.case.state, record).is_none());
    assert!(accepted.warnings.is_empty());
}

#[test]
fn frozen_basis_inputs_feed_freshness_edges() {
    let mut study = study("unseen_declared");
    let held = study.case.observe(study.held_out, DIGEST_H, None);
    let basis = study.basis(json!([{"input_ref": held, "holdout": "within_reserved"}]));
    let payload = study.assessment(held, basis);
    let accepted = study.case.submit("assessment.add", payload).unwrap();
    let record = accepted.records.last().unwrap();
    let edges = crate::freshness::dependency_edges(record).unwrap();
    let fields: Vec<&str> = edges.iter().map(|edge| edge.field).collect();
    assert!(fields.contains(&"frozen_basis.plan_ref"));
    assert!(fields.contains(&"frozen_basis.actual_inputs.input_ref"));
    // Outside the promotion closure: review read-sets are unchanged.
    let closure = crate::freshness::closure_links(record).unwrap();
    assert!(!closure.contains(&study.plan));
}
