//! The review-vault export (W11.07) on one small case built through real
//! operations and ingests:
//!
//! - a public Decision whose decisive objection is restricted;
//! - a public Conclusion with nothing withheld behind it;
//! - an internal Conclusion resting on a restricted observation;
//! - a restricted original and two reviewed derivatives of it, cleared for
//!   the public by reviewed downgrades: one plain, one whose bytes quote a
//!   record ID.
//!
//! Every restricted string carries [`RESTRICTED`], every internal one
//! [`INTERNAL`], so a byte scan of every planned file name, file and the
//! manifest finds any channel that leaks.

use std::{collections::BTreeMap, fs, path::Path};

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience,
    export::{
        Caveat, ExportError, ExportPlan, ExportSnapshot, Pin, Product, Retained, kind, plan_export,
        plan_export_with, review_vault, source::read_retained, write_export,
    },
};
use investigation_store::{Store, Workspace, store::ingest::IngestReceipt};
use serde_json::{Value, json};

const ACTOR: &str = "agent:review-vault-test";
const REVIEWER: &str = "human:review-vault-reviewer";
const RESTRICTED: &str = "ZEBRACODE";
const INTERNAL: &str = "OTTERNOTE";
const PUBLIC: &str = "PUFFINLINE";
const DERIVED: &str = "Redacted capture PUFFINLINE: the queue limit is 25 ms.\n";

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

fn ingest(
    store: &mut Store,
    root: &Path,
    key: &str,
    case: Id,
    contents: &str,
    level: &str,
) -> IngestReceipt {
    let path = root.join(format!("{key}.txt"));
    fs::write(&path, contents).unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":key, "actor":ACTOR, "case_id":case,
        "source":{"title":format!("{key} source"), "format":"text/plain"},
        "paths":[path], "classification":level, "extract_text":false
    });
    store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"))
}

/// A reviewed derivative of `original` holding `contents`, cleared for the
/// public by a reviewed downgrade that lists the original and its source.
#[allow(clippy::too_many_arguments)]
fn derivative(
    store: &mut Store,
    root: &Path,
    key: &str,
    case: Id,
    original: Id,
    original_digest: &Value,
    source: Id,
    contents: &str,
) -> Id {
    let path = root.join(format!("{key}.txt"));
    fs::write(&path, contents).unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":key, "actor":ACTOR, "case_id":case,
        "source":{"title":format!("{key} source"), "format":"text/plain"},
        "paths":[path], "classification":"public", "extract_text":false,
        "derived_from":{
            "inputs":[{"artifact_ref":{"id":original, "revision":1}, "digest":original_digest}],
            "transformation":"example-org.redact", "version":"2",
            "redaction_decisions":[{"scope":"Codenames", "action":"removed",
                "rationale":"Not needed to read the limit."}],
            "review":{"method":"Read the whole output against the original.",
                "coverage":"whole_output"}
        }
    });
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"));
    let derived = receipt.items[0].artifact_id;
    let digest = json!(receipt.items[0].digest);
    submit_as(
        store,
        &format!("{key}-downgrade"),
        REVIEWER,
        case,
        "classification.downgrade",
        json!({
            "subject_ref":{"id":derived, "revision":1}, "level":"public",
            "reason":"The codename was removed and the result reviewed.",
            "transformation":{
                "inputs":[{"input_ref":{"id":original, "revision":1}, "digest":original_digest},
                    {"input_ref":{"id":source, "revision":1}, "digest":null}],
                "transformation":"example-org.redact", "version":"2",
                "redaction_decisions":[{"scope":"Codenames", "action":"removed",
                    "rationale":"Not needed to read the limit."}],
                "output_digest":digest
            },
            "review":{"method":"Read the whole output.", "coverage":"whole_output",
                "examined":[{"id":derived, "revision":1}]}
        }),
    );
    derived
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    decision: Id,
    objection: Id,
    clean: Id,
    internal_on_restricted: Id,
    restricted_observation: Id,
    original: Id,
    original_bytes: Vec<u8>,
    derived: Id,
    quoting: Id,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, true).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let store_ref = &mut store;
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":"case", "case_id":null,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":"case.create",
            "payload":{"title":format!("{PUBLIC} case title"), "mode":"evaluate"}}]
    });
    let case = store_ref
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id;
    clear(store_ref, "clear-case", case, case);
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
    let clean = conclusion(
        store_ref,
        "clean",
        case,
        format!("{PUBLIC} clean conclusion"),
        &[],
    );
    clear(store_ref, "clear-clean", case, clean);
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

    // Evidence: a restricted original and two reviewed public derivatives.
    let original_text = format!("Codename {RESTRICTED} ships first.\nThe queue limit is 25 ms.\n");
    let secret = ingest(
        store_ref,
        &root,
        "original",
        case,
        &original_text,
        "restricted",
    );
    let (source, original) = (secret.items[0].source_id, secret.items[0].artifact_id);
    let original_digest = json!(secret.items[0].digest);
    let derived = derivative(
        store_ref,
        &root,
        "redacted",
        case,
        original,
        &original_digest,
        source,
        DERIVED,
    );
    let quoting = derivative(
        store_ref,
        &root,
        "quoting",
        case,
        original,
        &original_digest,
        source,
        &format!("This capture cites record {decision} by its ID.\n"),
    );
    Fixture {
        _directory: directory,
        store,
        case,
        decision,
        objection,
        clean,
        internal_on_restricted,
        restricted_observation,
        original,
        original_bytes: original_text.into_bytes(),
        derived,
        quoting,
    }
}

fn plan(fixture: &mut Fixture, audience: Audience) -> ExportPlan {
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    let digests = review_vault::attachment_digests(&snapshot, audience);
    let retained = read_retained(&fixture.store, &digests).unwrap();
    plan_export_with(
        kind("review-vault").unwrap(),
        &snapshot,
        audience,
        &retained,
    )
    .unwrap()
}

/// Every planned file by path, the manifest included.
fn files(plan: &ExportPlan) -> BTreeMap<String, Vec<u8>> {
    let mut out: BTreeMap<String, Vec<u8>> = plan
        .files()
        .entries()
        .iter()
        .map(|entry| (entry.path().as_str().to_owned(), entry.content().to_vec()))
        .collect();
    out.insert(
        "export-manifest.json".to_owned(),
        plan.manifest_bytes().to_vec(),
    );
    out
}

/// The ID the export shows for `id`: itself, or its opaque value when the
/// plan remaps identifiers (public).
fn shown(plan: &ExportPlan, id: Id) -> String {
    let canonical = id.to_string();
    plan.identifiers()
        .exported(&canonical)
        .unwrap_or_else(|| panic!("{canonical} has an exported value"))
        .to_owned()
}

fn note(plan: &ExportPlan, case: Id, folder: &str, id: Id) -> String {
    let path = format!(
        "Cases/{}/{folder}/{}.md",
        shown(plan, case),
        shown(plan, id)
    );
    String::from_utf8(
        plan.files()
            .entry(&path)
            .unwrap_or_else(|| panic!("{path} is planned"))
            .content()
            .to_vec(),
    )
    .unwrap()
}

/// Lowercase forms a record ID can take in a file: prefixed, bare, digits.
fn forms(id: Id) -> [String; 2] {
    [
        id.uuid().hyphenated().to_string(),
        id.uuid().simple().to_string(),
    ]
}

/// No withheld record is named in any path or byte, no text above the
/// audience appears, and no restricted original's bytes are copied.
fn assert_nothing_withheld_leaks(plan: &ExportPlan, fixture: &Fixture, above: &[&str]) {
    let all = files(plan);
    let original_digest = investigation_store::journal::hash_bytes(&fixture.original_bytes);
    for (path, bytes) in &all {
        let text = format!("{path}\n{}", String::from_utf8_lossy(bytes)).to_ascii_lowercase();
        for exclusion in plan.exclusions() {
            if exclusion.id == fixture.case {
                continue;
            }
            for form in forms(exclusion.id) {
                assert!(
                    !text.contains(&form),
                    "{path} names withheld {}",
                    exclusion.id
                );
            }
        }
        for needle in above {
            assert!(
                !text.contains(&needle.to_ascii_lowercase()),
                "{path} holds {needle}"
            );
        }
        assert_ne!(
            investigation_store::journal::hash_bytes(bytes),
            original_digest,
            "{path} is the original"
        );
    }
}

#[test]
fn internal_shows_standing_from_the_full_case_with_fixed_caveats_and_no_leak() {
    let mut fixture = fixture();
    let plan = plan(&mut fixture, Audience::Internal);
    assert_eq!(plan.kind().product, Product::Review);
    assert!(!plan.manifest().resumable);
    assert!(!plan.complete());
    let withheld: Vec<Id> = plan.exclusions().iter().map(|e| e.id).collect();
    assert!(withheld.contains(&fixture.objection));
    assert!(withheld.contains(&fixture.restricted_observation));
    assert!(withheld.contains(&fixture.original));

    // The decisive objection is withheld, yet the decision reads disputed,
    // and its note carries the fixed caveat word for word.
    let decision = note(&plan, fixture.case, "Decisions", fixture.decision);
    assert!(decision.contains("disputed"), "{decision}");
    assert!(
        decision.contains(&format!(
            "- Audience caveat: {}",
            Caveat::ChallengeWithheld.text()
        )),
        "{decision}"
    );
    let resting = note(
        &plan,
        fixture.case,
        "Conclusions",
        fixture.internal_on_restricted,
    );
    assert!(
        resting.contains(&format!(
            "- Audience caveat: {}",
            Caveat::BasisWithheld.text()
        )),
        "{resting}"
    );
    let clean = note(&plan, fixture.case, "Conclusions", fixture.clean);
    assert!(!clean.contains("Audience caveat"), "{clean}");
    // A view that lists a caveated record carries its caveat too.
    let decisions = String::from_utf8(
        plan.files()
            .entry(&format!("Cases/{}/Views/Decisions.md", fixture.case))
            .unwrap()
            .content()
            .to_vec(),
    )
    .unwrap();
    assert!(
        decisions.contains("## Audience caveats")
            && decisions.contains(Caveat::ChallengeWithheld.text()),
        "{decisions}"
    );

    assert_nothing_withheld_leaks(&plan, &fixture, &[RESTRICTED]);
    // No working-vault folder, Bases or Canvas file is planned.
    for entry in plan.files().entries() {
        let path = entry.path().as_str();
        for refused in ["Notes/", "Boards/", "Inbox/", ".base", ".canvas"] {
            assert!(!path.contains(refused), "{path}");
        }
    }
}

#[test]
fn public_shows_only_records_with_nothing_withheld_behind_them() {
    let mut fixture = fixture();
    let plan = plan(&mut fixture, Audience::Public);
    let withheld: Vec<Id> = plan.exclusions().iter().map(|e| e.id).collect();
    assert!(withheld.contains(&fixture.decision));
    assert!(withheld.contains(&fixture.internal_on_restricted));
    assert!(plan.caveated().is_empty());
    let clean = note(&plan, fixture.case, "Conclusions", fixture.clean);
    assert!(clean.contains(PUBLIC), "{clean}");
    for (path, bytes) in files(&plan) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains("Audience caveat"),
            "{path}"
        );
    }
    assert_nothing_withheld_leaks(&plan, &fixture, &[RESTRICTED, INTERNAL]);
    // Public identifiers are remapped, so no note tells how the journal
    // grew: no acceptance sequence or record time.
    for (path, bytes) in files(&plan) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("| Accepted at |") && !text.contains("inv_updated"),
            "{path}"
        );
    }
    let timeline = String::from_utf8(
        plan.files()
            .entry(&format!(
                "Cases/{}/Views/Timeline.md",
                shown(&plan, fixture.case)
            ))
            .unwrap()
            .content()
            .to_vec(),
    )
    .unwrap();
    assert!(timeline.contains("not shown in this export"), "{timeline}");
    // Internal keeps them.
    let internal = self::plan(&mut fixture, Audience::Internal);
    let clean = note(&internal, fixture.case, "Conclusions", fixture.clean);
    assert!(clean.contains("| Accepted at |"), "{clean}");
}

#[test]
fn only_reviewed_derivatives_free_of_identifiers_are_attached_byte_for_byte() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let plan = plan(&mut fixture, audience);
        let attachments: Vec<&str> = plan
            .files()
            .entries()
            .iter()
            .map(|entry| entry.path().as_str())
            .filter(|path| path.starts_with("Attachments/"))
            .collect();
        let expected = format!("Attachments/{}.txt", shown(&plan, fixture.derived));
        assert_eq!(attachments, [expected.as_str()], "{audience:?}");
        let entry = plan.files().entry(&expected).unwrap();
        assert_eq!(entry.content(), DERIVED.as_bytes());
        // The derivative quoting a record ID is shown but not attached, and
        // the omitted-data note says why beside its link.
        assert!(
            plan.files()
                .entry(&format!(
                    "Attachments/{}.txt",
                    shown(&plan, fixture.quoting)
                ))
                .is_none()
        );
        let omitted = String::from_utf8(
            plan.files()
                .entry("Omitted Data.md")
                .unwrap()
                .content()
                .to_vec(),
        )
        .unwrap();
        assert!(omitted.contains("## Attachments not copied"), "{omitted}");
        assert!(
            omitted.contains(&shown(&plan, fixture.quoting)),
            "{omitted}"
        );
        let start = String::from_utf8(
            plan.files()
                .entry("Start Here.md")
                .unwrap()
                .content()
                .to_vec(),
        )
        .unwrap();
        assert!(
            start.contains(&expected.trim_end_matches(".txt").to_string()),
            "{start}"
        );
        // The original never travels, under any name, to any audience.
        let original_digest = investigation_store::journal::hash_bytes(&fixture.original_bytes);
        for entry in plan.files().entries() {
            assert_ne!(entry.sha256(), &original_digest, "{}", entry.path());
        }
    }
    // Without the retained bytes, nothing is attached and the note says so.
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    let plan = plan_export(kind("review-vault").unwrap(), &snapshot, Audience::Internal).unwrap();
    assert!(
        plan.files()
            .entries()
            .iter()
            .all(|entry| !entry.path().as_str().starts_with("Attachments/"))
    );
    let _ = Retained::none();
}

#[test]
fn the_same_snapshot_gives_the_same_bytes() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let first = plan(&mut fixture, audience);
        let second = plan(&mut fixture, audience);
        assert_eq!(files(&first), files(&second), "{audience:?}");
        assert_eq!(first.manifest().export_id, second.manifest().export_id);
    }
}

#[test]
fn a_review_vault_is_written_only_into_a_new_directory() {
    let mut fixture = fixture();
    let plan = plan(&mut fixture, Audience::Internal);
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert!(matches!(
        write_export(&plan, &empty),
        Err(ExportError::DestinationExists)
    ));
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
    let file = directory.path().join("file");
    fs::write(&file, b"keep").unwrap();
    assert!(matches!(
        write_export(&plan, &file),
        Err(ExportError::DestinationExists)
    ));
    assert_eq!(fs::read(&file).unwrap(), b"keep");
    let fresh = directory.path().join("fresh");
    let written = write_export(&plan, &fresh).unwrap();
    assert!(written.created_directory);
    assert_eq!(written.files, plan.files().entries().len() + 1);
    assert!(matches!(
        write_export(&plan, &fresh),
        Err(ExportError::DestinationExists)
    ));
}
