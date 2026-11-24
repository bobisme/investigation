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
//!
//! Identifiers (W11.13): a public export, or one with remapping asked for,
//! shows no canonical ID, journal sequence, head hash or UUIDv7 creation
//! time, consistently remaps every record it shows, and the publisher's
//! mapping restores the canonical text.

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience,
    export::{
        Caveat, Completeness, ExportPlan, ExportSnapshot, IdentifierMode, KINDS, Pin, Retained,
        Scope, Shortfall, Statements, kind, plan_export, plan_export_remap, producer_bundle,
        review_vault, source::read_retained,
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

/// The report as the publisher reads it: with canonical IDs, also when the
/// export remapped them.
fn report(plan: &ExportPlan) -> String {
    String::from_utf8(plan.identifiers().restore(raw_report(plan).as_bytes())).unwrap()
}

/// The report's bytes as written.
fn raw_report(plan: &ExportPlan) -> String {
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
        if exclusion.id == case && plan.identifiers().mode() == IdentifierMode::Canonical {
            continue; // The case ID names the snapshot the export is of.
        }
        let canonical = exclusion.id.to_string();
        let found = mentions(plan, &canonical);
        assert!(found.is_empty(), "withheld {}: {found:#?}", exclusion.id);
        if exclusion.id != case
            && let Some(shown) = plan.identifiers().exported(&canonical)
        {
            let found = mentions(plan, shown);
            assert!(
                found.is_empty(),
                "withheld {} as {shown}: {found:#?}",
                exclusion.id
            );
        }
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
    // Every caveat line sits under a line naming one of the two caveated
    // records, wherever its standing appears, and is a fixed text.
    let caveated = [
        fixture.decision.to_string(),
        fixture.internal_on_restricted.to_string(),
    ];
    let mut named = "";
    for line in report.lines() {
        match line.trim_start().strip_prefix("- Caveat: ") {
            Some(text) => {
                assert!(
                    caveated.iter().any(|id| named.contains(id.as_str())),
                    "{named}\n{line}"
                );
                assert!(
                    [
                        Caveat::ChallengeWithheld.text(),
                        Caveat::BasisWithheld.text()
                    ]
                    .contains(&text)
                );
            }
            None => named = line,
        }
    }
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
    // sections count the shown records alone.
    let listed = |heading: &str| -> Vec<String> {
        report
            .split(heading)
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("- `"))
            .map(str::to_owned)
            .collect()
    };
    let mut section = listed("## Supported conclusions\n");
    section.extend(listed("## Decisions\n"));
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

/// Every bounded hyphenated UUIDv7 value in `text`, lowercased.
fn uuids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for start in 0..bytes.len().saturating_sub(35) {
        let candidate = &bytes[start..start + 36];
        let shaped = candidate
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                14 => *byte == b'7',
                _ => byte.is_ascii_hexdigit(),
            });
        let bounded = (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
            && bytes
                .get(start + 36)
                .is_none_or(|byte| !byte.is_ascii_alphanumeric());
        if shaped && bounded {
            found.push(String::from_utf8_lossy(candidate).to_ascii_lowercase());
        }
    }
    found
}

/// Every needle that betrays a canonical identity: each record and
/// workspace ID in its full, bare, upper-case and 32-digit forms and its
/// millisecond time prefix, and each head hash of the case's verified chain.
fn canonical_needles(fixture: &mut Fixture) -> Vec<String> {
    let replay = fixture.store.case(fixture.case).unwrap();
    let mut ids: Vec<Id> = replay
        .state
        .records()
        .map(|record| record.header.id)
        .collect();
    ids.push(fixture.store.workspace().id());
    let mut needles = Vec::new();
    for id in ids {
        let full = id.to_string();
        let uuid = full.split_once('_').unwrap().1.to_owned();
        needles.push(uuid[..13].to_owned());
        needles.push(uuid.replace('-', ""));
        needles.push(uuid.to_ascii_uppercase());
        needles.push(uuid);
        needles.push(full);
    }
    for sequence in 0..=replay.snapshot.sequence {
        let head = replay.state.verified_snapshot(sequence).unwrap();
        needles.push(head.as_str().trim_start_matches("sha256:").to_owned());
    }
    needles
}

/// A public export, at the head and at an earlier snapshot, names no
/// canonical ID, time prefix, journal sequence or head hash in any file
/// name, body or manifest byte; it still shows its public records, under
/// remapped IDs that the publisher's mapping turns back into case records.
#[test]
fn a_public_export_shows_no_canonical_identifier_sequence_or_time() {
    let mut fixture = fixture();
    let needles = canonical_needles(&mut fixture);
    let records: Vec<String> = {
        let replay = fixture.store.case(fixture.case).unwrap();
        replay
            .state
            .records()
            .map(|record| record.header.id.to_string())
            .collect()
    };
    let head = fixture.store.case(fixture.case).unwrap().snapshot;
    for pin in [
        Pin::default(),
        Pin {
            sequence: Some(fixture.before_raise),
            head_hash: None,
        },
    ] {
        let (_, plan) = export(&mut fixture, Audience::Public, &pin);
        let identifiers = plan.identifiers();
        assert_eq!(identifiers.mode(), IdentifierMode::Remapped);
        assert_eq!(plan.manifest().identifiers, Some(IdentifierMode::Remapped));
        assert_eq!(plan.manifest().snapshot.sequence, 0);
        assert_eq!(&plan.manifest().snapshot, identifiers.exported_snapshot());
        assert_eq!(identifiers.canonical_snapshot().case_id, fixture.case);
        let pinned = pin.sequence.unwrap_or(head.sequence);
        assert_eq!(identifiers.canonical_snapshot().sequence, pinned);
        let identifiers_statement = &plan.manifest().statements.identifiers;
        assert!(
            identifiers_statement.contains("remapped"),
            "{identifiers_statement}"
        );
        assert!(!identifiers_statement.contains("embed their creation time"));
        let mut shown = 0;
        for (path, text) in bytes(&plan) {
            for needle in &needles {
                assert!(!path.contains(needle.as_str()), "{path} names {needle}");
                assert!(!text.contains(needle.as_str()), "{path} contains {needle}");
            }
            for sequence in [pinned, head.sequence] {
                for form in [
                    format!("sequence {sequence}"),
                    format!("\"sequence\": {sequence}"),
                    format!("--snapshot {sequence}"),
                ] {
                    assert!(!text.contains(&form), "{path} contains {form}");
                }
            }
            // Every ID shown is a remapped one whose canonical value is a
            // record of the case.
            for uuid in uuids(&text) {
                let canonical = identifiers
                    .canonical(&uuid)
                    .unwrap_or_else(|| panic!("{path}: {uuid} is not in the mapping"));
                assert!(
                    records.iter().any(|record| record.ends_with(canonical)),
                    "{path}: {uuid} maps to {canonical}, not a record"
                );
                shown += 1;
            }
        }
        // The clean public conclusion is shown, remapped, and the mapping
        // finds it again.
        let clean = fixture.clean.to_string();
        let remapped = identifiers
            .exported(&clean)
            .expect("the clean conclusion is shown");
        assert_ne!(remapped, clean);
        assert!(raw_report(&plan).contains(remapped));
        assert!(listing(&report(&plan), fixture.clean).is_some());
        assert!(shown >= 3, "{shown}");
    }
}

/// Internal and restricted exports keep canonical IDs and say that they
/// embed their creation time, unless remapping is asked for. A remapped
/// export restores to the canonical one through the publisher's mapping,
/// except for the lines that name the snapshot; the mapping is a bijection,
/// deterministic, and differs between audiences of one snapshot.
#[test]
fn internal_keeps_canonical_ids_unless_remapping_is_asked_and_restores_exactly() {
    let mut fixture = fixture();
    let spec = kind("case-report").unwrap();
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    for audience in [Audience::Internal, Audience::Restricted] {
        let canonical = plan_export(spec, &snapshot, audience).unwrap();
        let manifest = canonical.manifest();
        assert_eq!(manifest.identifiers, Some(IdentifierMode::Canonical));
        assert_eq!(manifest.snapshot.case_id, fixture.case);
        assert_eq!(manifest.snapshot.sequence, snapshot.case().head().sequence);
        assert_eq!(
            manifest.statements.identifiers,
            "Record IDs are UUIDv7 values and embed their creation time to the millisecond."
        );
        assert_eq!(canonical.identifiers().len(), 0);
        let text = raw_report(&canonical);
        assert!(text.contains(&fixture.clean.to_string()));

        let remapped =
            plan_export_remap(spec, &snapshot, audience, &Retained::none(), true).unwrap();
        let again = plan_export_remap(spec, &snapshot, audience, &Retained::none(), true).unwrap();
        assert_eq!(bytes(&remapped), bytes(&again));
        assert_eq!(remapped.identifiers(), again.identifiers());
        assert_eq!(remapped.manifest().snapshot.sequence, 0);
        assert_eq!(remapped.exclusions(), canonical.exclusions());
        let pairs: Vec<(&str, &str)> = remapped.identifiers().pairs().collect();
        for (from, to) in &pairs {
            assert_ne!(from, to);
            assert_eq!(remapped.identifiers().canonical(to), Some(*from));
        }
        let restored = report(&remapped);
        let names_snapshot = |line: &&str| {
            line.starts_with("| Snapshot |")
                || line.starts_with("| Bundle |")
                || line.contains("full evidence chain")
        };
        let kept: Vec<&str> = restored
            .lines()
            .filter(|line| !names_snapshot(line))
            .collect();
        let expected: Vec<&str> = text.lines().filter(|line| !names_snapshot(line)).collect();
        assert_eq!(kept, expected, "{audience:?}");
        // The manifest's references map back to the canonical manifest's.
        let restored_manifest = remapped.identifiers().restore(remapped.manifest_bytes());
        let restored_manifest: serde_json::Value =
            serde_json::from_slice(&restored_manifest).unwrap();
        let canonical_manifest: serde_json::Value =
            serde_json::from_slice(canonical.manifest_bytes()).unwrap();
        assert_eq!(
            restored_manifest["snapshot"]["case_id"],
            canonical_manifest["snapshot"]["case_id"]
        );
        assert_eq!(restored_manifest["sources"], canonical_manifest["sources"]);
        assert_eq!(
            restored_manifest["supersession"],
            canonical_manifest["supersession"]
        );
    }
    // One record gets unrelated values in exports for different audiences.
    let clean = fixture.clean.to_string();
    let public = plan_export(spec, &snapshot, Audience::Public).unwrap();
    let internal =
        plan_export_remap(spec, &snapshot, Audience::Internal, &Retained::none(), true).unwrap();
    assert_ne!(
        public.identifiers().exported(&clean),
        internal.identifiers().exported(&clean)
    );
    assert_ne!(public.manifest().snapshot, internal.manifest().snapshot);
}

/// For every case kind and audience, the manifest reads complete exactly
/// when nothing was withheld and nothing was left out, and its statement is
/// the fixed one for that shortfall: the same words whether or not an
/// omission stands beside withheld records (bn-1vg8).
#[test]
fn every_kind_reads_complete_only_when_nothing_was_withheld_or_left_out() {
    let mut fixture = fixture();
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    for spec in KINDS
        .iter()
        .filter(|spec| spec.available() && spec.scope() == Scope::Case)
    {
        for audience in [Audience::Restricted, Audience::Internal, Audience::Public] {
            let mut digests = review_vault::attachment_digests(&snapshot, audience);
            digests.extend(producer_bundle::required_digests(
                snapshot.case().state(),
                snapshot.today(),
            ));
            let retained = read_retained(&fixture.store, &digests).unwrap();
            for remap in [false, true] {
                let label = format!("{} {audience:?} remap {remap}", spec.name);
                let plan = plan_export_remap(spec, &snapshot, audience, &retained, remap)
                    .unwrap_or_else(|error| panic!("{label}: {error}"));
                let withheld = !plan.exclusions().is_empty();
                let omitted = !plan.omissions().is_empty();
                assert_eq!(plan.complete(), !withheld && !omitted, "{label}");
                let manifest = plan.manifest();
                assert_eq!(
                    manifest.completeness == Completeness::Complete,
                    plan.complete(),
                    "{label}: {:?}",
                    plan.omissions()
                );
                let shortfall = Shortfall::of(withheld, omitted);
                assert_eq!(
                    manifest.statements.completeness,
                    Statements::fixed(shortfall, audience, plan.identifiers().mode()).completeness,
                    "{label}"
                );
            }
        }
    }
}

#[test]
fn every_report_kind_keeps_withheld_material_out() {
    let mut fixture = fixture();
    for name in ["case-report", "analysis-report"] {
        for (audience, above) in [
            (Audience::Internal, vec![RESTRICTED]),
            (Audience::Public, vec![RESTRICTED, INTERNAL]),
        ] {
            let snapshot =
                ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
            let plan = plan_export(kind(name).unwrap(), &snapshot, audience).unwrap();
            assert!(!plan.complete(), "{name}");
            assert_nothing_withheld_leaks(&plan, fixture.case, &above);
        }
    }
}

/// A case whose public decisions each select an option withheld from the
/// audience under test: one internal option (withheld from `public`) and one
/// raised to restricted after the decisions were recorded (withheld from
/// `internal` too). A decision's title is derived from its chosen option, so
/// this is where a withheld title could leak through another record's title
/// (bn-31an). Also returns the sequence before the raise: a re-export pinned
/// there sees the option as internal in the snapshot's own classification,
/// while today's classification withholds it.
fn withheld_option_case() -> (tempfile::TempDir, Store, Id, [Id; 2], u64) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":"case", "case_id":null,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":"case.create",
            "payload":{"title":format!("{PUBLIC} case title"), "mode":"evaluate"}}]
    });
    let case = store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id;
    let s = &mut store;
    clear(s, "clear-case", case, case);
    let question = create(
        s,
        "question",
        case,
        "question.create",
        json!({"wording":format!("{PUBLIC} question wording"), "importance":"normal"}),
    );
    clear(s, "clear-question", case, question);
    let mut decisions = [case; 2];
    let mut options = [case; 2];
    for (index, marker) in [INTERNAL, RESTRICTED].into_iter().enumerate() {
        let question_ref = current(s, case, question);
        let option = create(
            s,
            &format!("option-{index}"),
            case,
            "option.create",
            json!({"question_ref":question_ref,
                "description":format!("{marker} withheld option title"),
                "dependencies":[], "tradeoff_notes":[]}),
        );
        options[index] = option;
        let question_ref = current(s, case, question);
        let option_ref = current(s, case, option);
        let decision = create(
            s,
            &format!("decision-{index}"),
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
        clear(s, &format!("clear-decision-{index}"), case, decision);
        decisions[index] = decision;
    }
    let before_raise = s.case(case).unwrap().snapshot.sequence;
    raise(s, "raise-option", case, options[1]);
    (directory, store, case, decisions, before_raise)
}

/// bn-31an: a decision's derived title never carries the title of a
/// withheld option, in any report kind or audience. A shown decision whose
/// option is withheld gets the neutral, ID-free label instead.
#[test]
fn a_withheld_option_title_never_reaches_a_report_through_a_decision_title() {
    let (_directory, mut store, case, decisions, before_raise) = withheld_option_case();
    let mut labelled = 0;
    let pins = [
        Pin::default(),
        Pin {
            sequence: Some(before_raise),
            head_hash: None,
        },
    ];
    for (name, pin) in ["case-report", "analysis-report"]
        .into_iter()
        .flat_map(|name| pins.iter().map(move |pin| (name, pin)))
    {
        let snapshot = ExportSnapshot::read(&mut store, case, pin).unwrap();
        // With everything shown the derived title names the option, so the
        // marker would appear if it were not withheld.
        let plan = plan_export(kind(name).unwrap(), &snapshot, Audience::Restricted).unwrap();
        if name == "case-report" {
            let shown = listing(&report(&plan), decisions[1]).unwrap();
            assert!(shown.contains(RESTRICTED), "{shown}");
        }
        let pinned = pin.sequence.is_some();
        // The decisions whose option each audience does not see.
        for (audience, above, withheld) in [
            (Audience::Internal, vec![RESTRICTED], &decisions[1..]),
            (Audience::Public, vec![RESTRICTED, INTERNAL], &decisions[..]),
        ] {
            let plan = plan_export(kind(name).unwrap(), &snapshot, audience).unwrap();
            assert!(!plan.complete(), "{name} pinned {pinned}");
            assert_nothing_withheld_leaks(&plan, case, &above);
            if name == "case-report" {
                let text = report(&plan);
                for &decision in withheld {
                    if let Some(shown) = listing(&text, decision) {
                        let line = shown.lines().next().unwrap();
                        assert!(
                            line.starts_with(&format!("- `{decision}` Decision:")),
                            "{name} {audience:?} pinned {pinned}: {line}"
                        );
                        labelled += usize::from(pinned);
                    }
                }
            }
        }
    }
    // Not vacuous: in the pinned re-export the snapshot's own classification
    // still shows the option, so only the inclusion keeps its title out.
    assert!(labelled > 0);
}
