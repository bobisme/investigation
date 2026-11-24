//! The handoff export (W11.03) under the audience policy, on one small case
//! built through real operations:
//!
//! - a public Decision whose decisive (critical) objection is restricted;
//! - a public Conclusion with nothing withheld behind it;
//! - the newest Conclusion, public but resting on an internal Observation, so
//!   it leads the case for internal and restricted readers and is withheld
//!   for the public one;
//! - Runs with an unknown outcome at internal and at restricted level, and a
//!   restricted open Assumption.
//!
//! Every restricted string carries [`RESTRICTED`] and every internal one
//! [`INTERNAL`], so a byte scan of both files, the manifest and the file
//! names finds any channel that leaks.

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience,
    export::{
        Caveat, ExportPlan, ExportSnapshot, IdentifierMode, OmissionReason, Omitted, Pin, Retained,
        kind, plan_export, plan_export_remap,
    },
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

fn run_record(store: &mut Store, key: &str, case: Id, intent: String) -> Id {
    create(
        store,
        key,
        case,
        "run.record",
        json!({
            "intent":{"kind":"exploration", "intent":intent},
            "producer":"agent:export-policy-test", "artifact_refs":[],
            "execution_status":"unknown", "evidence_completeness":"partial",
            "evidence_kind":"observational", "variables_changed":[]
        }),
    )
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    decision: Id,
    clean: Id,
    leading: Id,
    internal_run: Id,
    restricted_run: Id,
    restricted_assumption: Id,
    /// Public Decisions choosing an internal and a restricted Option.
    chose_withheld: Vec<Id>,
    /// The case sequence before the second option was raised to restricted.
    before_late_raise: u64,
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
            "payload":{"title":format!("{INTERNAL} case title"), "mode":"evaluate",
                "objective":format!("{INTERNAL} objective")}}]
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
    raise(store_ref, "raise-objection", case, objection);

    // Public Decisions choosing an internal option, and an option raised to
    // restricted only after the last pinned snapshot: their derived titles
    // ("Chosen option: ...") must never borrow a withheld option's text.
    let mut chose_withheld = Vec::new();
    let mut late_option = None;
    for (key, level) in [
        ("internal-option", INTERNAL),
        ("restricted-option", RESTRICTED),
    ] {
        let question_ref = current(store_ref, case, question);
        let hidden = create(
            store_ref,
            key,
            case,
            "option.create",
            json!({"question_ref":question_ref,
                "description":format!("{level} option description"),
                "dependencies":[], "tradeoff_notes":[]}),
        );
        if level == RESTRICTED {
            late_option = Some(hidden);
        }
        let question_ref = current(store_ref, case, question);
        let hidden_ref = current(store_ref, case, hidden);
        let chooser = create(
            store_ref,
            &format!("{key}-decision"),
            case,
            "decision.propose",
            json!({
                "question_ref":question_ref,
                "choice":{"kind":"select", "option_ref":hidden_ref},
                "compared_alternatives":[hidden_ref],
                "comparison_refs":[], "hard_conditions":[], "evidence_refs":[],
                "assumption_refs":[], "tradeoffs":["A plain tradeoff."], "consequences":["A plain consequence."],
                "reopen_trigger":"Reopen on new evidence.", "reversal_conditions":[]
            }),
        );
        clear(store_ref, &format!("clear-{key}-decision"), case, chooser);
        chose_withheld.push(chooser);
    }
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
    let leading = conclusion(
        store_ref,
        "leading",
        case,
        format!("{PUBLIC} leading conclusion on internal evidence"),
        &[internal_observation],
    );
    clear(store_ref, "clear-leading", case, leading);
    let internal_run = run_record(store_ref, "internal-run", case, format!("{INTERNAL} run"));
    let restricted_run = run_record(
        store_ref,
        "restricted-run",
        case,
        format!("{RESTRICTED} run"),
    );
    raise(store_ref, "raise-run", case, restricted_run);
    let restricted_assumption = create(
        store_ref,
        "assumption",
        case,
        "assumption.create",
        json!({"claim":format!("{RESTRICTED} assumption claim"),
            "consequence":format!("{RESTRICTED} assumption consequence"),
            "materiality":"material", "affected_refs":[]}),
    );
    raise(store_ref, "raise-assumption", case, restricted_assumption);
    let before_late_raise = store_ref.case(case).unwrap().snapshot.sequence;
    raise(store_ref, "raise-late-option", case, late_option.unwrap());
    Fixture {
        _directory: directory,
        store,
        case,
        decision,
        clean,
        leading,
        internal_run,
        restricted_run,
        restricted_assumption,
        chose_withheld,
        before_late_raise,
    }
}

fn export(fixture: &mut Fixture, audience: Audience) -> ExportPlan {
    export_at(fixture, audience, &Pin::default())
}

fn export_at(fixture: &mut Fixture, audience: Audience, pin: &Pin) -> ExportPlan {
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, pin).unwrap();
    plan_export(kind("handoff").unwrap(), &snapshot, audience).unwrap()
}

fn file(plan: &ExportPlan, path: &str) -> String {
    String::from_utf8(plan.files().entry(path).unwrap().content().to_vec()).unwrap()
}

/// One file with every remapped ID and the bundle head put back to their
/// canonical values, as the publisher reads it; the file as written when
/// canonical.
fn restored(plan: &ExportPlan, path: &str) -> String {
    String::from_utf8(plan.identifiers().restore(file(plan, path).as_bytes())).unwrap()
}

/// Every file name and its text, the manifest included.
fn everything(plan: &ExportPlan) -> String {
    let mut out = String::from_utf8(plan.manifest_bytes().to_vec()).unwrap();
    for entry in plan.files().entries() {
        out.push_str(entry.path().as_str());
        out.push('\n');
        out.push_str(std::str::from_utf8(entry.content()).unwrap());
    }
    out
}

/// The lines of one section, up to the next heading of any level.
fn section(text: &str, heading: &str) -> String {
    text.lines()
        .skip_while(|line| *line != heading)
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line naming `id` in `text` and the caveat lines under it.
fn listing(text: &str, id: Id) -> Option<String> {
    let id = id.to_string();
    let mut lines = text
        .lines()
        .skip_while(|line| !(line.starts_with("- ") && line.contains(&id)));
    let first = lines.next()?;
    let mut out = vec![first];
    out.extend(lines.take_while(|line| line.trim_start().starts_with("- Caveat: ")));
    Some(out.join("\n"))
}

/// Checks the restored text, so a remapped export is held to the same
/// canonical IDs.
fn assert_nothing_withheld_leaks(plan: &ExportPlan, case: Id, above: &[&str]) {
    let text = String::from_utf8(plan.identifiers().restore(everything(plan).as_bytes())).unwrap();
    for exclusion in plan.exclusions() {
        if exclusion.id != case {
            assert!(
                !text.contains(&exclusion.id.to_string()),
                "withheld {} ({}) is named",
                exclusion.id,
                exclusion.reason
            );
        }
    }
    for needle in above {
        let found: Vec<&str> = text.lines().filter(|line| line.contains(needle)).collect();
        assert!(found.is_empty(), "{needle}: {found:#?}");
    }
}

#[test]
fn restricted_sees_every_run_and_the_leading_outcome() {
    let mut fixture = fixture();
    let plan = export(&mut fixture, Audience::Restricted);
    assert!(plan.complete());
    assert!(plan.omissions().is_empty(), "{:?}", plan.omissions());
    for path in ["handoff-brief.md", "handoff-overview.md"] {
        let text = file(&plan, path);
        assert!(!text.contains("- Caveat: "), "{path}");
        let heading = if path == "handoff-brief.md" {
            "## Do not repeat until reconciled"
        } else {
            "### Do not repeat until reconciled"
        };
        let runs = section(&text, heading);
        for run in [fixture.internal_run, fixture.restricted_run] {
            assert!(runs.contains(&run.to_string()), "{path}: {runs}");
        }
        assert!(
            text.contains(&format!("Leading outcome `{}`", fixture.leading)),
            "{path}"
        );
        assert!(
            text.contains(&fixture.restricted_assumption.to_string()),
            "{path}"
        );
        // The objection bears on the decision, not on the leading outcome:
        // the decision reads disputed and needs no caveat at this level.
        let decision = listing(&text, fixture.decision).unwrap();
        assert!(
            decision.contains("effective `disputed`"),
            "{path}: {decision}"
        );
    }
}

#[test]
fn internal_caveats_the_challenged_decision_and_names_nothing_restricted() {
    let mut fixture = fixture();
    let plan = export(&mut fixture, Audience::Internal);
    assert!(!plan.complete());
    assert_nothing_withheld_leaks(&plan, fixture.case, &[RESTRICTED]);
    // The gaps left out because they read withheld records are reported to
    // the publisher (bn-1vg8); the restricted run's own gap is one of them.
    assert!(
        plan.omissions()
            .iter()
            .any(|omission| omission.content == Omitted::Gap
                && omission.record == Some(fixture.restricted_run)
                && omission.reason == OmissionReason::RestsOnWithheld),
        "{:?}",
        plan.omissions()
    );
    for omission in plan.omissions() {
        assert!(
            matches!(
                omission.content,
                Omitted::Gap | Omitted::Finding | Omitted::NextStep
            ),
            "{omission:?}"
        );
    }
    for path in ["handoff-brief.md", "handoff-overview.md"] {
        let text = file(&plan, path);
        // The decision's decisive objection is withheld; its state is
        // computed with it and the fixed caveat stands beside it.
        let decision = listing(&text, fixture.decision).unwrap();
        assert!(
            decision.contains("effective `disputed`"),
            "{path}: {decision}"
        );
        assert!(
            decision.contains(Caveat::ChallengeWithheld.text()),
            "{path}: {decision}"
        );
        let clean = listing(&text, fixture.clean).unwrap();
        assert!(!clean.contains("Caveat"), "{path}: {clean}");
        assert!(text.contains(&fixture.internal_run.to_string()), "{path}");
        assert!(
            text.contains(&format!("Leading outcome `{}`", fixture.leading)),
            "{path}"
        );
        assert!(text.contains(&format!("{INTERNAL} objective")), "{path}");
        // The decision choosing the internal option borrows its title here.
        let chooser = listing(&text, fixture.chose_withheld[0]).unwrap();
        assert!(
            chooser.contains(&format!("{INTERNAL} option")),
            "{path}: {chooser}"
        );
    }
}

#[test]
fn public_never_promotes_a_weaker_record_into_a_withheld_place() {
    let mut fixture = fixture();
    let plan = export(&mut fixture, Audience::Public);
    assert!(!plan.complete());
    assert_nothing_withheld_leaks(&plan, fixture.case, &[RESTRICTED, INTERNAL]);
    let canonical = plan.identifiers().canonical_snapshot();
    let raw = everything(&plan);
    assert!(!raw.contains(hex(canonical.head_hash.as_str())));
    assert!(!raw.contains(&fixture.case.to_string()));
    for path in ["handoff-brief.md", "handoff-overview.md"] {
        let written = file(&plan, path);
        // A remapped export names the bundle and opens nothing.
        assert!(written.contains("- Bundle: `sha256:"), "{path}");
        assert!(!written.contains("sequence "), "{path}");
        assert!(!written.contains("--snapshot"), "{path}");
        assert!(!written.contains("investigation --case"), "{path}");
        assert!(
            written.contains("title, summary, full and evidence available to the publisher"),
            "{path}"
        );
        let text = restored(&plan, path);
        // The newest conclusion rests on internal evidence and is withheld;
        // the older clean one is listed but does not take its place.
        assert!(
            text.contains("- Leading outcome: None recorded or included."),
            "{path}"
        );
        assert!(listing(&text, fixture.clean).is_some(), "{path}");
        assert!(!text.contains("leading outcome\n"), "{path}");
        assert!(!text.contains("- Caveat: "), "{path}");
        assert!(
            text.contains(&format!("{PUBLIC} question wording")),
            "{path}"
        );
        assert!(
            text.contains("No run with an incomplete or unknown outcome is recorded or included."),
            "{path}"
        );
    }
}

/// Remapping an internal handoff keeps what it says and drops the journal:
/// restored, it names the same records, but it prints no sequence, no case
/// head and no command a workspace could run.
#[test]
fn a_remapped_handoff_names_the_bundle_and_leaves_reads_to_the_publisher() {
    let mut fixture = fixture();
    let canonical = export(&mut fixture, Audience::Internal);
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    let spec = kind("handoff").unwrap();
    let plan =
        plan_export_remap(spec, &snapshot, Audience::Internal, &Retained::none(), true).unwrap();
    assert_eq!(plan.identifiers().mode(), IdentifierMode::Remapped);
    let head = plan.identifiers().canonical_snapshot().head_hash.clone();
    assert_eq!(head, canonical.manifest().snapshot.head_hash);
    for path in ["handoff-brief.md", "handoff-overview.md"] {
        let written = file(&plan, path);
        assert!(!written.contains(hex(head.as_str())), "{path}");
        assert!(!written.contains("sequence "), "{path}");
        assert!(!written.contains("--snapshot"), "{path}");
        assert!(!written.contains("investigation --case"), "{path}");
        assert!(written.contains("- Bundle: `sha256:"), "{path}");
        let text = restored(&plan, path);
        assert!(
            text.contains(&format!("Leading outcome `{}`", fixture.leading)),
            "{path}"
        );
        assert!(text.contains(&fixture.internal_run.to_string()), "{path}");
        let escalation = |text: &str| {
            text.split("## Escalation")
                .nth(1)
                .unwrap()
                .lines()
                .filter_map(|line| line.strip_prefix("- `"))
                .filter_map(|line| line.split('`').next())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            escalation(&text),
            escalation(&file(&canonical, path)),
            "{path}"
        );
    }
}

#[test]
fn the_same_snapshot_and_audience_give_the_same_bytes() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let first = export(&mut fixture, audience);
        let second = export(&mut fixture, audience);
        assert_eq!(everything(&first), everything(&second));
        assert_eq!(first.manifest().export_id, second.manifest().export_id);
    }
}

/// At an earlier snapshot the second option was still internal, so the
/// working-vault title rules would compose "Chosen option: <its text>" for
/// the decision that chose it. Today it is restricted: the decision is
/// shown to the internal audience with its basis caveat, and with no title
/// borrowed from the withheld option.
#[test]
fn a_derived_title_never_borrows_a_withheld_options_text() {
    let mut fixture = fixture();
    let pin = Pin {
        sequence: Some(fixture.before_late_raise),
        head_hash: None,
    };
    let plan = export_at(&mut fixture, Audience::Internal, &pin);
    assert_nothing_withheld_leaks(&plan, fixture.case, &[RESTRICTED]);
    let chooser = fixture.chose_withheld[1];
    assert_eq!(
        plan.caveated()
            .iter()
            .find(|caveated| caveated.id == chooser)
            .map(|caveated| caveated.caveats.clone()),
        Some(vec![Caveat::BasisWithheld])
    );
    for path in ["handoff-brief.md", "handoff-overview.md"] {
        let text = file(&plan, path);
        let line = listing(&text, chooser).unwrap();
        assert!(!line.contains("Chosen option"), "{path}: {line}");
        assert!(
            line.contains(Caveat::BasisWithheld.text()),
            "{path}: {line}"
        );
    }
}

/// The hex digits of a `sha256:` digest.
fn hex(digest: &str) -> &str {
    digest.trim_start_matches("sha256:")
}
