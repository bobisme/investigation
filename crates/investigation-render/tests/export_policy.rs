//! The export audience policy (`inv.export-policy.v2`, W11.02) on one small
//! case built through real operations.
//!
//! - A public Decision whose decisive (critical) objection is restricted.
//! - A public Conclusion with nothing withheld behind it.
//! - A public Conclusion resting on an internal observation.
//! - An internal Conclusion resting on a restricted observation.
//! - A public hypothesis whose only assessment is public but rests on an
//!   internal observation, so withholding cascades from the assessment to
//!   the hypothesis it challenges.
//!
//! Every restricted string carries [`RESTRICTED`], every internal one
//! [`INTERNAL`], so a byte scan of every planned file, the manifest and the
//! file names finds any channel that leaks: names, counts, hashes and the
//! omission manifest.

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience,
    export::{Caveat, ExportPlan, ExportSnapshot, Pin, kind, plan_export},
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:export-policy-test";
const REVIEWER: &str = "human:export-reviewer";
const RESTRICTED: &str = "ZEBRACODE";
const INTERNAL: &str = "OTTERNOTE";
const PUBLIC: &str = "PUFFINLINE";

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn submit_as(
    store: &mut Store,
    key: &str,
    actor: &str,
    case: Id,
    op: &str,
    payload: Value,
) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":actor, "preconditions":[], "operations":[{"op":op, "payload":payload}]
    });
    let receipt = store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"));
    assert_eq!(
        receipt.result,
        CommitResult::Committed,
        "{key}: {receipt:?}"
    );
    receipt
}

fn create(store: &mut Store, key: &str, case: Id, op: &str, payload: Value) -> Id {
    submit_as(store, key, ACTOR, case, op, payload).created[0].id
}

fn current(store: &mut Store, case: Id, id: Id) -> Value {
    let replay = store.case(case).unwrap();
    let revision = replay.state.current(id).unwrap().header.revision.0;
    reference(id, revision)
}

fn raise(store: &mut Store, key: &str, case: Id, id: Id) {
    let subject = current(store, case, id);
    submit_as(
        store,
        key,
        ACTOR,
        case,
        "classification.raise",
        json!({"subject_ref":subject, "level":"restricted",
            "reason":format!("{RESTRICTED} raise reason {key}")}),
    );
}

/// Clear a record for the public audience by a reviewed downgrade.
fn clear(store: &mut Store, key: &str, case: Id, id: Id) {
    let subject = current(store, case, id);
    submit_as(
        store,
        key,
        REVIEWER,
        case,
        "classification.downgrade",
        json!({
            "subject_ref":subject, "level":"public",
            "reason":"Reviewed for publication.",
            "transformation":{
                "inputs":[{"input_ref":subject, "digest":null}],
                "transformation":"manual_review", "version":"1",
                "redaction_decisions":[{"scope":"Whole record", "action":"retained",
                    "rationale":"Nothing in it is sensitive."}],
                "output_digest":null
            },
            "review":{"method":"Read the whole record.", "coverage":"whole_output",
                "examined":[subject]}
        }),
    );
}

fn observation(store: &mut Store, key: &str, case: Id, statement: String) -> Id {
    create(
        store,
        key,
        case,
        "observation.add",
        json!({
            "value":{"kind":"text", "statement":statement},
            "basis":{"kind":"assertion", "assertion":format!("{statement} assertion"), "context":null},
            "extraction_method":null, "validity":"valid", "quality":[], "limitations":[],
            "evidence_kind":"observational", "variables_changed":[], "observed_at":null
        }),
    )
}

fn conclusion(store: &mut Store, key: &str, case: Id, statement: String, support: &[Id]) -> Id {
    let support: Vec<Value> = support.iter().map(|id| reference(*id, 1)).collect();
    create(
        store,
        key,
        case,
        "conclusion.create",
        json!({
            "statement":statement, "scope":"Scope of the statement.",
            "significance":"Significance of the statement.",
            "support_refs":support, "contradiction_refs":[], "caveats":[],
            "causal_factors":[], "confidence":null, "likelihood":null
        }),
    )
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    decision: Id,
    objection: Id,
    clean: Id,
    public_on_internal: Id,
    internal_on_restricted: Id,
    restricted_observation: Id,
    hypothesis: Id,
    assessment: Id,
    /// The case journal sequence before the objection was raised.
    before_raise: u64,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let store_ref = &mut store;
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":"case", "case_id":null,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":"case.create",
            "payload":{"title":format!("{INTERNAL} case title"), "mode":"evaluate"}}]
    });
    let case = store_ref
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id;
    let question = create(
        store_ref,
        "question",
        case,
        "question.create",
        json!({"wording":format!("{PUBLIC} question wording"), "importance":"normal"}),
    );
    clear(store_ref, "clear-question", case, question);

    // The public decision and its restricted decisive objection.
    let question_ref = current(store_ref, case, question);
    let option = create(
        store_ref,
        "option",
        case,
        "option.create",
        json!({"question_ref":question_ref,
            "description":format!("{PUBLIC} option description"),
            "dependencies":[], "tradeoff_notes":[]}),
    );
    clear(store_ref, "clear-option", case, option);
    let question_ref = current(store_ref, case, question);
    let option_ref = current(store_ref, case, option);
    let decision = create(
        store_ref,
        "decision",
        case,
        "decision.propose",
        json!({
            "question_ref":question_ref,
            "choice":{"kind":"select", "option_ref":option_ref},
            "compared_alternatives":[option_ref],
            "comparison_refs":[], "hard_conditions":[], "evidence_refs":[],
            "assumption_refs":[], "tradeoffs":[format!("{PUBLIC} tradeoff")],
            "consequences":[format!("{PUBLIC} consequence")],
            "reopen_trigger":format!("{PUBLIC} reopen trigger"), "reversal_conditions":[]
        }),
    );
    clear(store_ref, "clear-decision", case, decision);
    let decision_ref = current(store_ref, case, decision);
    let objection = create(
        store_ref,
        "objection",
        case,
        "objection.raise",
        json!({"subject_ref":decision_ref,
            "statement":format!("{RESTRICTED} objection statement"),
            "scope":format!("{RESTRICTED} objection scope"), "severity":"critical",
            "evidence_refs":{"state":"unknown"}}),
    );

    // Conclusions with and without withheld material behind them.
    let clean = conclusion(
        store_ref,
        "clean",
        case,
        format!("{PUBLIC} clean conclusion"),
        &[],
    );
    clear(store_ref, "clear-clean", case, clean);
    let internal_observation = observation(
        store_ref,
        "internal-observation",
        case,
        format!("{INTERNAL} observation"),
    );
    let public_on_internal = conclusion(
        store_ref,
        "public-on-internal",
        case,
        format!("{PUBLIC} conclusion on internal evidence"),
        &[internal_observation],
    );
    clear(
        store_ref,
        "clear-public-on-internal",
        case,
        public_on_internal,
    );
    let restricted_observation = observation(
        store_ref,
        "restricted-observation",
        case,
        format!("{RESTRICTED} observation"),
    );
    raise(store_ref, "raise-observation", case, restricted_observation);
    let internal_on_restricted = conclusion(
        store_ref,
        "internal-on-restricted",
        case,
        format!("{INTERNAL} conclusion on restricted evidence"),
        &[restricted_observation],
    );

    // A public hypothesis challenged only by a public assessment that rests
    // on internal evidence.
    let question_ref = current(store_ref, case, question);
    let hypothesis = create(
        store_ref,
        "hypothesis",
        case,
        "hypothesis.create",
        json!({"question_ref":question_ref,
            "statement":format!("{PUBLIC} hypothesis statement"),
            "assumption_refs":[], "rival_refs":[],
            "applicability":format!("{PUBLIC} applicability"),
            "possible_falsifiers":[format!("{PUBLIC} falsifier")]}),
    );
    clear(store_ref, "clear-hypothesis", case, hypothesis);
    let hypothesis_ref = current(store_ref, case, hypothesis);
    let assessment = create(
        store_ref,
        "assessment",
        case,
        "assessment.add",
        json!({
            "subject_ref":hypothesis_ref,
            "evidence_refs":[reference(internal_observation, 1)],
            "relation":"inconclusive", "rationale":format!("{PUBLIC} assessment rationale"),
            "rival_expectations":[], "assumption_refs":[],
            "confidence":null, "likelihood":null
        }),
    );
    clear(store_ref, "clear-assessment", case, assessment);

    let before_raise = store_ref.case(case).unwrap().snapshot.sequence;
    raise(store_ref, "raise-objection", case, objection);
    Fixture {
        _directory: directory,
        store,
        case,
        decision,
        objection,
        clean,
        public_on_internal,
        internal_on_restricted,
        restricted_observation,
        hypothesis,
        assessment,
        before_raise,
    }
}

fn export(fixture: &mut Fixture, audience: Audience, pin: &Pin) -> (ExportSnapshot, ExportPlan) {
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, pin).unwrap();
    let plan = plan_export(kind("case-report").unwrap(), &snapshot, audience).unwrap();
    (snapshot, plan)
}

/// Every file name and line of the export, the manifest included.
fn bytes(plan: &ExportPlan) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = plan
        .files()
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.path().as_str().to_owned(),
                String::from_utf8(entry.content().to_vec()).unwrap(),
            )
        })
        .collect();
    out.push((
        "export-manifest.json".to_owned(),
        String::from_utf8(plan.manifest_bytes().to_vec()).unwrap(),
    ));
    out
}

fn mentions(plan: &ExportPlan, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (path, text) in bytes(plan) {
        if path.contains(needle) {
            found.push(format!("path {path}"));
        }
        found.extend(
            text.lines()
                .filter(|line| line.contains(needle))
                .map(|line| format!("{path}: {line}")),
        );
    }
    found
}

fn report(plan: &ExportPlan) -> String {
    String::from_utf8(
        plan.files()
            .entry("case-report.md")
            .unwrap()
            .content()
            .to_vec(),
    )
    .unwrap()
}

/// The report line naming `id` and the caveat lines under it.
fn listing(report: &str, id: Id) -> Option<String> {
    let id = id.to_string();
    let mut lines = report.lines().skip_while(|line| !line.contains(&id));
    let first = lines.next()?;
    let mut out = vec![first];
    out.extend(lines.take_while(|line| line.starts_with("  - Caveat: ")));
    Some(out.join("\n"))
}

fn reason(plan: &ExportPlan, id: Id) -> Option<&'static str> {
    plan.exclusions()
        .iter()
        .find(|exclusion| exclusion.id == id)
        .map(|exclusion| exclusion.reason)
}

fn caveats(plan: &ExportPlan, id: Id) -> Vec<Caveat> {
    plan.caveated()
        .iter()
        .find(|caveated| caveated.id == id)
        .map(|caveated| caveated.caveats.clone())
        .unwrap_or_default()
}

/// No withheld record is named, counted or hashed anywhere in the export,
/// and no text above the audience appears in any byte or file name.
fn assert_nothing_withheld_leaks(plan: &ExportPlan, case: Id, above: &[&str]) {
    for exclusion in plan.exclusions() {
        if exclusion.id == case {
            continue; // The case ID names the snapshot the export is of.
        }
        let found = mentions(plan, &exclusion.id.to_string());
        assert!(found.is_empty(), "withheld {}: {found:#?}", exclusion.id);
    }
    for needle in above {
        let found = mentions(plan, needle);
        assert!(found.is_empty(), "{needle}: {found:#?}");
    }
}

#[test]
fn restricted_sees_everything_with_no_caveat() {
    let mut fixture = fixture();
    let (_, plan) = export(&mut fixture, Audience::Restricted, &Pin::default());
    assert!(plan.complete(), "{:?}", plan.exclusions());
    assert!(plan.caveated().is_empty());
    let report = report(&plan);
    assert!(!report.contains("- Caveat: "));
    let decision = listing(&report, fixture.decision).unwrap();
    assert!(decision.contains("effective `disputed`"), "{decision}");
    for id in [
        fixture.clean,
        fixture.public_on_internal,
        fixture.internal_on_restricted,
        fixture.restricted_observation,
    ] {
        assert!(listing(&report, id).is_some(), "{id}");
    }
}

#[test]
fn internal_carries_the_fixed_caveat_where_restricted_material_bears() {
    let mut fixture = fixture();
    let (_, plan) = export(&mut fixture, Audience::Internal, &Pin::default());
    assert!(!plan.complete());
    assert_eq!(reason(&plan, fixture.objection), Some("audience"));
    assert_eq!(
        reason(&plan, fixture.restricted_observation),
        Some("audience")
    );
    // Only the restricted records and their raises are withheld: every
    // dependent record is shown, with its caveat.
    for id in [
        fixture.decision,
        fixture.clean,
        fixture.public_on_internal,
        fixture.internal_on_restricted,
        fixture.hypothesis,
        fixture.assessment,
    ] {
        assert_eq!(reason(&plan, id), None, "{id}");
    }
    assert_eq!(
        caveats(&plan, fixture.decision),
        [Caveat::ChallengeWithheld]
    );
    assert_eq!(
        caveats(&plan, fixture.internal_on_restricted),
        [Caveat::BasisWithheld]
    );
    assert_eq!(
        plan.caveated().len(),
        2,
        "nothing else bears on withheld material: {:?}",
        plan.caveated()
    );

    let report = report(&plan);
    // The decisive objection is withheld, yet the decision does not read
    // clean: its state is computed with the objection and the caveat says so.
    let decision = listing(&report, fixture.decision).unwrap();
    assert!(decision.contains("effective `disputed`"), "{decision}");
    assert!(
        decision.contains(Caveat::ChallengeWithheld.text()),
        "{decision}"
    );
    let resting = listing(&report, fixture.internal_on_restricted).unwrap();
    assert!(resting.contains(Caveat::BasisWithheld.text()), "{resting}");
    let clean = listing(&report, fixture.clean).unwrap();
    assert!(!clean.contains("Caveat"), "{clean}");
    assert_eq!(report.matches("- Caveat: ").count(), 2);
    assert_nothing_withheld_leaks(&plan, fixture.case, &[RESTRICTED]);
    let manifest = &plan.manifest();
    assert_eq!(manifest.policy.version, "inv.export-policy.v2");
    assert!(manifest.statements.disclosure.contains("fixed caveat"));
}

#[test]
fn public_withholds_every_record_that_bears_on_withheld_material() {
    let mut fixture = fixture();
    let (_, plan) = export(&mut fixture, Audience::Public, &Pin::default());
    assert!(!plan.complete());
    // The public decision whose decisive objection is restricted is withheld,
    // since the public audience is told of no omission per record.
    assert_eq!(reason(&plan, fixture.decision), Some("challenge_withheld"));
    assert_eq!(
        reason(&plan, fixture.public_on_internal),
        Some("basis_withheld")
    );
    // The assessment rests on internal evidence; the hypothesis it
    // challenges is withheld in turn.
    assert_eq!(reason(&plan, fixture.assessment), Some("basis_withheld"));
    assert_eq!(
        reason(&plan, fixture.hypothesis),
        Some("challenge_withheld")
    );
    assert_eq!(
        reason(&plan, fixture.internal_on_restricted),
        Some("audience")
    );
    assert_eq!(reason(&plan, fixture.clean), None);
    assert!(plan.caveated().is_empty(), "{:?}", plan.caveated());

    let report = report(&plan);
    assert!(listing(&report, fixture.clean).is_some());
    assert!(!report.contains("- Caveat: "));
    // The only conclusion or decision listed is the clean one, so the
    // section's count is the count of shown records alone.
    let section: Vec<&str> = report
        .split("## Conclusions and decisions")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("- "))
        .collect();
    assert_eq!(section.len(), 1, "{section:?}");
    assert_nothing_withheld_leaks(&plan, fixture.case, &[RESTRICTED, INTERNAL]);
    assert!(plan.manifest().sources.is_empty());
    assert!(
        plan.manifest()
            .statements
            .disclosure
            .contains("is withheld as well")
    );
}

#[test]
fn re_exporting_an_old_snapshot_follows_todays_policy() {
    let mut fixture = fixture();
    let pin = Pin {
        sequence: Some(fixture.before_raise),
        head_hash: None,
    };
    // At the pinned snapshot the objection was internal; today it is
    // restricted, so an internal re-export withholds it and caveats the
    // decision it challenges.
    let (snapshot, plan) = export(&mut fixture, Audience::Internal, &pin);
    assert!(snapshot.historical());
    assert_eq!(reason(&plan, fixture.objection), Some("audience_today"));
    assert_eq!(
        caveats(&plan, fixture.decision),
        [Caveat::ChallengeWithheld]
    );
    let decision = listing(&report(&plan), fixture.decision).unwrap();
    assert!(decision.contains("effective `disputed`"), "{decision}");
    assert!(
        decision.contains(Caveat::ChallengeWithheld.text()),
        "{decision}"
    );
    assert_nothing_withheld_leaks(&plan, fixture.case, &[RESTRICTED]);

    // The same pinned snapshot for public: the decision is withheld.
    let (_, public) = export(&mut fixture, Audience::Public, &pin);
    assert_eq!(
        reason(&public, fixture.decision),
        Some("challenge_withheld")
    );
    assert_nothing_withheld_leaks(&public, fixture.case, &[RESTRICTED, INTERNAL]);

    // Restricted sees the objection at the pinned snapshot and needs no
    // caveat.
    let (_, restricted) = export(&mut fixture, Audience::Restricted, &pin);
    assert!(restricted.complete());
    assert!(restricted.caveated().is_empty());
}

#[test]
fn the_same_inputs_give_the_same_decisions_and_bytes() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let (_, first) = export(&mut fixture, audience, &Pin::default());
        let (_, second) = export(&mut fixture, audience, &Pin::default());
        assert_eq!(first.exclusions(), second.exclusions());
        assert_eq!(first.caveated(), second.caveated());
        assert_eq!(bytes(&first), bytes(&second));
    }
}
