//! Classification-aware projection: the audience policy on every vault
//! channel.
//!
//! One case mixes internal records with records raised to `restricted`
//! through the real `classification.raise` operation: a rival hypothesis, an
//! observation read from a restricted acquisition, an assessment, an
//! annotation, a critical objection against a visible conclusion, an option
//! a visible decision chooses, a criterion a visible comparison measures,
//! and a comparison. A second case is restricted as a whole. Every restricted
//! string carries [`RESTRICTED`] and every internal string [`INTERNAL`], so a
//! byte scan of the whole plan (paths and contents) finds any channel that
//! leaks: bodies, titles, aliases, frontmatter, filenames, link labels,
//! backlink reasons, tables, counts, the Overview and `Start Here.md`.

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
};

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience, RenderPlan, VaultRenderer, VerifiedSnapshot, ViewPolicy, layout, render, render_vault,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};
use support::vault;

const ACTOR: &str = "agent:audience-test";
/// Prefix of every restricted string: it must never reach a vault rendered
/// for the `internal` or `public` audience.
const RESTRICTED: &str = "ZEBRACODE";
/// Prefix of every internal string: it must never reach a `public` vault.
const INTERNAL: &str = "OTTERNOTE";
/// A restricted comparison's measured value.
const RESTRICTED_VALUE: &str = "424242";
/// A restricted run's sample count.
const RESTRICTED_SAMPLES: u64 = 137;

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn submit(store: &mut Store, key: &str, case: Option<Id>, op: &str, payload: Value) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":op, "payload":payload}]
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
    submit(store, key, Some(case), op, payload).created[0].id
}

/// The current revision of a record.
fn revision(store: &mut Store, case: Id, id: Id) -> u64 {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    snapshot
        .case(case)
        .unwrap()
        .state()
        .current(id)
        .unwrap()
        .header
        .revision
        .0
}

/// Raise a record's current revision to `restricted`, with a reason that is
/// itself restricted text.
fn raise(store: &mut Store, key: &str, case: Id, id: Id) {
    let revision = revision(store, case, id);
    submit(
        store,
        key,
        Some(case),
        "classification.raise",
        json!({"subject_ref":reference(id, revision), "level":"restricted",
            "reason":format!("{RESTRICTED} raise reason {key}")}),
    );
}

fn ingest(store: &mut Store, root: &Path, key: &str, case: Id, text: &str, level: &str) -> Value {
    let path = root.join(format!("{key}.txt"));
    fs::write(&path, text).unwrap();
    let marker = if level == "restricted" {
        RESTRICTED
    } else {
        INTERNAL
    };
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":key, "actor":ACTOR, "case_id":case,
        "source":{"title":format!("{marker} source {key}"), "format":"text/plain"},
        "paths":[path], "classification":level, "extract_text":true
    });
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    serde_json::to_value(&receipt.items[0]).unwrap()
}

fn observation(artifact: Id, digest: &Value, statement: &str, quote: &str) -> Value {
    json!({
        "value":{"kind":"text", "statement":statement},
        "basis":{"kind":"source", "artifact_ref":reference(artifact, 1),
            "selector":{"schema_version":"inv.selector.v1", "artifact_digest":digest,
                "selector":{"type":"text_lines", "start_line":2, "end_line":2,
                    "encoding":"utf-8", "parser_version":"inv.text.utf8.v1"}},
            "text_quote":{"exact":quote}},
        "validity":"valid", "quality":[], "limitations":[format!("{statement} limitation")],
        "evidence_kind":"observational", "variables_changed":[]
    })
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    secret_case: Id,
    factor: Id,
    assessment: Id,
    conclusion: Id,
    objection: Id,
    decision: Id,
    comparison: Id,
    measured: Id,
    /// Records (with their case) the `internal` audience must see as ID-only
    /// placeholders.
    restricted: Vec<(Id, Id)>,
}

#[allow(clippy::too_many_lines)]
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
    let mut owned = Store::open(Workspace::open(&root).unwrap());
    let store = &mut owned;
    let case = submit(
        store,
        "case",
        None,
        "case.create",
        json!({"title":format!("{INTERNAL} case title"), "mode":"explain"}),
    )
    .created[0]
        .id;
    let question = create(
        store,
        "question",
        case,
        "question.create",
        json!({"wording":format!("{INTERNAL} question wording"), "importance":"normal"}),
    );
    let hypothesis = |statement: String, rivals: Vec<Value>| {
        json!({"question_ref":reference(question, 1), "statement":statement,
            "assumption_refs":[], "rival_refs":rivals,
            "applicability":format!("{statement} applicability"),
            "possible_falsifiers":[format!("{statement} falsifier")]})
    };
    let rival = create(
        store,
        "rival",
        case,
        "hypothesis.create",
        hypothesis(format!("{RESTRICTED} rival statement"), vec![]),
    );
    let factor = create(
        store,
        "factor",
        case,
        "hypothesis.create",
        hypothesis(
            format!("{INTERNAL} factor statement"),
            vec![reference(rival, 1)],
        ),
    );

    // Evidence: one internal capture and one restricted acquisition.
    let capture = ingest(
        store,
        &root,
        "capture",
        case,
        &format!("start\n{INTERNAL} pool exhausted\n"),
        "internal",
    );
    let text: Id = capture["derivative_id"].as_str().unwrap().parse().unwrap();
    let seen = create(
        store,
        "observation",
        case,
        "observation.add",
        observation(
            text,
            &capture["digest"],
            &format!("{INTERNAL} observation statement"),
            &format!("{INTERNAL} pool exhausted"),
        ),
    );
    let plan = ingest(
        store,
        &root,
        "plan",
        case,
        &format!("start\n{RESTRICTED} rollout plan\n"),
        "restricted",
    );
    let plan_text: Id = plan["derivative_id"].as_str().unwrap().parse().unwrap();
    let secret_seen = create(
        store,
        "secret-observation",
        case,
        "observation.add",
        observation(
            plan_text,
            &plan["digest"],
            &format!("{RESTRICTED} observation statement"),
            &format!("{RESTRICTED} rollout plan"),
        ),
    );

    let assessment = create(
        store,
        "assessment",
        case,
        "assessment.add",
        json!({
            "subject_ref":reference(factor, 1),
            "evidence_refs":[reference(seen, 1), reference(secret_seen, 1)],
            "relation":"supports", "rationale":format!("{RESTRICTED} assessment rationale"),
            "rival_expectations":[{"rival_ref":reference(rival, 1), "expectation":"less_expected",
                "rationale":format!("{RESTRICTED} rival expectation")}],
            "assumption_refs":[],
            "confidence":{"level":"moderate", "rationale":format!("{RESTRICTED} confidence"),
                "limitations":[format!("{RESTRICTED} limitation")],
                "what_would_change_it":format!("{RESTRICTED} change"), "downgrade_reasons":null},
            "likelihood":null
        }),
    );
    let annotation = create(
        store,
        "annotation",
        case,
        "annotation.add",
        json!({"subject_refs":[reference(factor, 1)],
            "content":{"kind":"text", "text":format!("{RESTRICTED} annotation text")}}),
    );

    // Evaluation: a restricted option chosen by a visible decision, and a
    // restricted criterion measured by a visible comparison.
    let option = create(
        store,
        "option",
        case,
        "option.create",
        json!({"question_ref":reference(question, 1),
            "description":format!("{INTERNAL} option description"),
            "dependencies":[], "tradeoff_notes":[format!("{INTERNAL} tradeoff")]}),
    );
    let secret_option = create(
        store,
        "secret-option",
        case,
        "option.create",
        json!({"question_ref":reference(question, 1),
            "description":format!("{RESTRICTED} option description"),
            "dependencies":[], "tradeoff_notes":[format!("{RESTRICTED} tradeoff")]}),
    );
    let criterion = |name: String| {
        json!({"name":name, "kind":"hard",
            "scope":{"kind":"all_options", "question_ref":reference(question, 1)},
            "scale":{"unit":"ms", "description":format!("{name} scale"), "anchors":[]},
            "preferred_direction":"lower",
            "threshold":{"kind":"measured", "operator":"at_most",
                "value":{"decimal":"25", "unit":"ms"}}})
    };
    let secret_criterion = create(
        store,
        "secret-criterion",
        case,
        "criterion.create",
        criterion(format!("{RESTRICTED} criterion name")),
    );
    let comparison = |option: Id, value: &str, marker: &str| {
        json!({"option_ref":reference(option, 1), "criterion_ref":reference(secret_criterion, 1),
            "outcome":"meets", "method":format!("{marker} method"),
            "value":{"decimal":value, "unit":"ms"}, "evidence_refs":[seen],
            "assumption_refs":[], "uncertainty":format!("{marker} uncertainty"),
            "rationale":format!("{marker} comparison rationale")})
    };
    let visible_comparison = create(
        store,
        "comparison",
        case,
        "comparison.add",
        comparison(option, "18.5", INTERNAL),
    );
    let secret_comparison = create(
        store,
        "secret-comparison",
        case,
        "comparison.add",
        comparison(secret_option, &format!("21.{RESTRICTED_VALUE}"), RESTRICTED),
    );
    let eligible = submit(
        store,
        "eligible",
        Some(case),
        "option.disposition",
        json!({"option_ref":reference(secret_option, 1), "lifecycle":"eligible",
            "reason":format!("{RESTRICTED} eligibility reason")}),
    );
    let secret_option_revision = eligible.updated[0].revision;
    let decision = create(
        store,
        "decision",
        case,
        "decision.propose",
        json!({
            "question_ref":reference(question, 1),
            "choice":{"kind":"select", "option_ref":reference(secret_option, secret_option_revision)},
            "compared_alternatives":[reference(secret_option, secret_option_revision)],
            "comparison_refs":[], "hard_conditions":[], "evidence_refs":[],
            "assumption_refs":[], "tradeoffs":[format!("{INTERNAL} decision tradeoff")],
            "consequences":[format!("{INTERNAL} consequence")],
            "reopen_trigger":format!("{INTERNAL} reopen trigger"), "reversal_conditions":[]
        }),
    );

    // A visible measurement whose statistic and sample count come from a
    // restricted run: the measurement flag reads the run's fields.
    let run = create(
        store,
        "run",
        case,
        "run.record",
        json!({
            "intent":{"kind":"exploration", "intent":format!("{RESTRICTED} run intent")},
            "producer":"agent:producer", "artifact_refs":[], "execution_status":"completed",
            "evidence_completeness":"complete", "evidence_kind":"observational",
            "variables_changed":[],
            "measurement":{"statistic":{"state":"known", "value":{"kind":"quantile", "quantile":"0.99"}},
                "sample_count":{"state":"known", "value":RESTRICTED_SAMPLES}}
        }),
    );
    let measured = create(
        store,
        "measured",
        case,
        "observation.add",
        json!({
            "value":{"kind":"measurement", "value":"12", "unit":"ms"},
            "basis":{"kind":"assertion", "assertion":format!("{INTERNAL} reported"), "context":null},
            "extraction_method":null, "validity":"valid", "quality":[], "limitations":[],
            "evidence_kind":"observational", "variables_changed":[], "observed_at":null,
            "run_ref":reference(run, 1)
        }),
    );

    // Created last, so the conclusion is the answer the Overview and
    // Start Here lead with.
    let conclusion = create(
        store,
        "conclusion",
        case,
        "conclusion.create",
        json!({
            "statement":format!("{INTERNAL} conclusion statement"),
            "scope":format!("{INTERNAL} conclusion scope"),
            "significance":format!("{INTERNAL} significance"),
            "support_refs":[reference(seen, 1)], "contradiction_refs":[reference(rival, 1)],
            "caveats":[format!("{INTERNAL} caveat")], "causal_factors":[],
            "confidence":null, "likelihood":null
        }),
    );
    // The decisive objection: critical, against the visible conclusion.
    let objection = create(
        store,
        "objection",
        case,
        "objection.raise",
        json!({"subject_ref":reference(conclusion, 1),
            "statement":format!("{RESTRICTED} objection statement"),
            "scope":format!("{RESTRICTED} objection scope"), "severity":"critical",
            "evidence_refs":{"state":"unknown"}}),
    );
    for (key, id) in [
        ("raise-rival", rival),
        ("raise-assessment", assessment),
        ("raise-objection", objection),
        ("raise-annotation", annotation),
        ("raise-option", secret_option),
        ("raise-criterion", secret_criterion),
        ("raise-comparison", secret_comparison),
        ("raise-run", run),
    ] {
        raise(store, key, case, id);
    }

    // A second case restricted as a whole.
    let secret_case = submit(
        store,
        "secret-case",
        None,
        "case.create",
        json!({"title":format!("{RESTRICTED} case title"), "mode":"explain"}),
    )
    .created[0]
        .id;
    let secret_question = create(
        store,
        "secret-question",
        secret_case,
        "question.create",
        json!({"wording":format!("{RESTRICTED} second case question"), "importance":"normal"}),
    );
    raise(store, "raise-case", secret_case, secret_case);

    let plan_artifact: Id = plan["artifact_id"].as_str().unwrap().parse().unwrap();
    let mut restricted: Vec<(Id, Id)> = [
        rival,
        assessment,
        objection,
        annotation,
        secret_option,
        secret_criterion,
        secret_comparison,
        run,
        plan_artifact,
        plan_text,
        secret_seen,
    ]
    .into_iter()
    .map(|id| (case, id))
    .collect();
    restricted.push((secret_case, secret_question));
    Fixture {
        _directory: directory,
        store: owned,
        case,
        secret_case,
        factor,
        assessment,
        conclusion,
        objection,
        decision,
        comparison: visible_comparison,
        measured,
        restricted,
    }
}

fn plan(fixture: &mut Fixture, audience: Audience) -> RenderPlan {
    let snapshot = VerifiedSnapshot::read(&mut fixture.store).unwrap();
    render(&snapshot, &ViewPolicy::for_audience(audience)).unwrap()
}

fn text(plan: &RenderPlan, path: &str) -> String {
    String::from_utf8(plan.entry(path).unwrap().content().to_vec()).unwrap()
}

fn note(plan: &RenderPlan, case: Id, id: Id) -> String {
    let path = layout::record_note(case, id).unwrap().unwrap();
    text(plan, path.as_str())
}

/// Every vault byte that names `needle`, as `path: line`.
fn leaks(plan: &RenderPlan, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for entry in plan.entries() {
        if entry.path().as_str().contains(needle) {
            found.push(format!("path {}", entry.path()));
        }
        let content = String::from_utf8_lossy(entry.content());
        for line in content.lines().filter(|line| line.contains(needle)) {
            found.push(format!("{}: {line}", entry.path()));
        }
    }
    found
}

#[test]
fn no_vault_byte_carries_text_above_the_audience() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let plan = plan(&mut fixture, audience);
        let restricted = leaks(&plan, RESTRICTED);
        let restricted_value = leaks(&plan, RESTRICTED_VALUE);
        let internal = leaks(&plan, INTERNAL);
        // A restricted run's count, quoted by a flag and its gap.
        let samples = leaks(&plan, &format!("{RESTRICTED_SAMPLES} recorded samples"));
        assert_eq!(
            samples.is_empty(),
            audience != Audience::Restricted,
            "{audience:?}: {samples:#?}"
        );
        match audience {
            Audience::Public => {
                assert!(restricted.is_empty(), "public: {restricted:#?}");
                assert!(restricted_value.is_empty(), "public: {restricted_value:#?}");
                assert!(internal.is_empty(), "public: {internal:#?}");
            }
            Audience::Internal => {
                assert!(restricted.is_empty(), "internal: {restricted:#?}");
                assert!(
                    restricted_value.is_empty(),
                    "internal: {restricted_value:#?}"
                );
                // Not vacuous: internal text is shown to this audience.
                assert!(!internal.is_empty());
            }
            Audience::Restricted => {
                assert!(!restricted.is_empty() && !restricted_value.is_empty());
                assert!(!internal.is_empty());
            }
        }
        // Withheld records keep an ID-only note; nothing names them by title.
        let withheld = audience != Audience::Restricted;
        for &(case, id) in &fixture.restricted {
            let note = note(&plan, case, id);
            assert_eq!(
                vault::property(&note, "inv_withheld").is_some(),
                withheld,
                "{id} for {audience:?}\n{note}"
            );
            if withheld {
                assert_eq!(vault::property(&note, "aliases"), Some("[]".to_owned()));
                assert!(note.contains(&format!("\n# {id}\n")), "{note}");
            }
        }
        let overview = text(
            &plan,
            layout::case_overview(fixture.secret_case).unwrap().as_str(),
        );
        assert_eq!(
            vault::property(&overview, "inv_withheld").is_some(),
            withheld
        );
    }
}

#[test]
fn a_conclusion_whose_decisive_objection_is_hidden_is_never_shown_clean() {
    let mut fixture = fixture();
    let plan = plan(&mut fixture, Audience::Internal);
    let objection = fixture.objection.to_string();

    // The conclusion's own note: disputed, with the objection by ID only.
    let conclusion = note(&plan, fixture.case, fixture.conclusion);
    assert!(
        conclusion.contains("- Effective disposition: `disputed`"),
        "{conclusion}"
    );
    let strongest = conclusion
        .lines()
        .find(|line| line.starts_with("- Strongest outstanding objection: "))
        .unwrap();
    assert!(
        strongest.contains(&format!("|{objection}]]")),
        "{strongest}"
    );
    assert!(!strongest.contains("critical"), "{strongest}");
    assert_eq!(
        vault::property(&conclusion, "inv_disposition").as_deref(),
        Some("disputed")
    );

    // Its objections list keeps the hidden objection as a placeholder.
    let objections = vault::section(&conclusion, "Reviews and objections").unwrap();
    assert!(
        objections.contains(&format!(
            "|{objection}]]: details withheld by the view policy\n"
        )),
        "{objections}"
    );

    // A withheld assessment of a visible hypothesis may contradict it: it is
    // listed by ID on the hypothesis and counted on the Overview.
    let assessment = fixture.assessment.to_string();
    let factor = note(&plan, fixture.case, fixture.factor);
    assert!(
        factor.contains(&format!(
            "1 further assessment is withheld by the view policy: [[Cases/{}/Assessments/{assessment}|{assessment}]].\n",
            fixture.case
        )),
        "{factor}"
    );
    assert!(!factor.contains("No assessment relates any evidence"));

    // The Overview's answer carries the objection caveat, not the clean
    // recorded caveat.
    let overview = text(&plan, layout::case_overview(fixture.case).unwrap().as_str());
    let caveat = overview
        .lines()
        .find(|line| line.starts_with("- Material caveat: "))
        .unwrap_or_else(|| panic!("{overview}"));
    assert!(caveat.contains(&format!("|{objection}]]")), "{caveat}");
    assert!(!caveat.contains("recorded caveat"), "{caveat}");

    assert!(
        overview.contains("0 non-diagnostic, 1 withheld by the view policy"),
        "{overview}"
    );
    assert!(overview.contains("Withheld by the view policy (relation not shown):"));

    // Start Here names the answer with its effective, disputed state, and
    // says its lists leave withheld records out.
    let start = text(&plan, "Start Here.md");
    assert!(
        start.contains("  - Withheld by the view policy: "),
        "{start}"
    );
    assert!(
        start.contains(&format!(
            "|{INTERNAL} conclusion statement]], effective `disputed`"
        )),
        "{start}"
    );
}

#[test]
fn derived_titles_and_cells_never_name_a_withheld_record() {
    let mut fixture = fixture();
    let plan = plan(&mut fixture, Audience::Internal);
    // A decision's title is derived from its chosen option; the option is
    // withheld, so the decision is titled by its ID.
    let decision = note(&plan, fixture.case, fixture.decision);
    assert!(
        decision.contains(&format!("\n# {}\n", fixture.decision)),
        "{decision}"
    );
    // A visible comparison names its withheld criterion by ID, without the
    // criterion's threshold.
    let comparison = note(&plan, fixture.case, fixture.comparison);
    assert!(comparison.contains(&format!("{INTERNAL} comparison rationale")));
    assert!(!comparison.contains("Cited criterion"), "{comparison}");
}

#[test]
fn a_measurement_flag_that_reads_a_withheld_run_names_only_its_rule() {
    let mut fixture = fixture();
    let samples = format!("{RESTRICTED_SAMPLES} recorded samples");
    let internal = note(
        &plan(&mut fixture, Audience::Internal),
        fixture.case,
        fixture.measured,
    );
    let flags =
        vault::section(&internal, "Measurement flags").unwrap_or_else(|| panic!("{internal}"));
    assert!(
        flags.contains(
            "- `measurement.quantile_below_order_statistic_bound`: details withheld by the view policy; the rule read a withheld record."
        ),
        "{flags}"
    );
    assert!(!internal.contains(&samples), "{internal}");
    assert!(
        internal.contains(
            "- Gap `ready_to_analyze`: details withheld by the view policy; the rule read a withheld record. Rule `measurement.quantile_below_order_statistic_bound`."
        ),
        "{internal}"
    );
    let cleared = note(
        &plan(&mut fixture, Audience::Restricted),
        fixture.case,
        fixture.measured,
    );
    assert!(cleared.contains(&samples), "{cleared}");
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn vault_mentions(vault: &Path, needle: &str) -> Vec<String> {
    files_under(vault)
        .into_iter()
        .filter(|path| {
            path.to_string_lossy().contains(needle)
                || String::from_utf8_lossy(&fs::read(path).unwrap()).contains(needle)
        })
        .map(|path| path.display().to_string())
        .collect()
}

#[test]
fn narrowing_the_audience_and_rendering_again_removes_what_was_shown() {
    let mut fixture = fixture();
    let vault = fixture.store.workspace().root().join("vault");
    let cleared = ViewPolicy::for_audience(Audience::Restricted);
    render_vault(&mut fixture.store, &VaultRenderer, &cleared, &mut |_| {}).unwrap();
    assert!(!vault_mentions(&vault, RESTRICTED).is_empty());

    let report = render_vault(
        &mut fixture.store,
        &VaultRenderer,
        &ViewPolicy::default(),
        &mut |_| {},
    )
    .unwrap();
    assert!(report.drift.is_empty(), "{:?}", report.drift);
    assert!(report.unknown.is_empty(), "{:?}", report.unknown);
    let left = vault_mentions(&vault, RESTRICTED);
    assert!(left.is_empty(), "{left:#?}");
    assert!(vault_mentions(&vault, RESTRICTED_VALUE).is_empty());

    render_vault(
        &mut fixture.store,
        &VaultRenderer,
        &ViewPolicy::for_audience(Audience::Public),
        &mut |_| {},
    )
    .unwrap();
    let left = vault_mentions(&vault, INTERNAL);
    assert!(left.is_empty(), "{left:#?}");
}
