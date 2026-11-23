//! The optional example packs `source-research`, `decision-analysis` and
//! `security-research` (W09.09): each loads and pins through the public
//! contracts with no core change, none is a default, and the security pack
//! carries guidance text only.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_core::method_pack::load;
use serde_json::{Value, json};

const ACTOR: &str = "agent:research-packs";
const SOURCE_RESEARCH: &str = include_str!("../../../methods/source-research.toml");
const DECISION_ANALYSIS: &str = include_str!("../../../methods/decision-analysis.toml");
const SECURITY_RESEARCH: &str = include_str!("../../../methods/security-research.toml");

fn output(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let result = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert_eq!(
        result.status.code(),
        Some(exit),
        "{args:?}\n{stdout}\n{:?}",
        String::from_utf8_lossy(&result.stderr)
    );
    stdout
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let stdout = output(cwd, args, stdin, exit);
    serde_json::from_str(&stdout).expect(&stdout)
}

struct Case<'a> {
    root: &'a Path,
    id: String,
    attachment: Value,
}

impl Case<'_> {
    fn submit(&self, key: &str, actor: &str, operation: Value, pre: Value, exit: i32) -> Value {
        let command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":actor, "preconditions":pre, "operations":[operation]
        });
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let receipt = self.submit(key, ACTOR, json!({"op":op,"payload":payload}), json!([]), 0);
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    }

    fn show(&self, id: &str) -> Value {
        run(
            self.root,
            &["--case", &self.id, "show", id, "--depth", "full", "--json"],
            None,
            0,
        )
    }

    /// Retain a pack file through ingest and pin it; returns its digest.
    fn adopt(&self, pack_id: &str, text: &str) -> String {
        let file = format!("{pack_id}.toml");
        fs::write(self.root.join(&file), text).unwrap();
        let retained = run(
            self.root,
            &["ingest", "--input", "-", "--json"],
            Some(&json!({
                "schema_version": "inv.ingest.v1", "command_id": format!("retain-{pack_id}"),
                "actor": ACTOR, "case_id": self.id,
                "source": {"title": pack_id, "format": "application/toml"}, "paths": [file]
            })),
            0,
        );
        let item = &retained["items"][0];
        let digest = item["digest"].as_str().unwrap().to_owned();
        let revision = self.show(&self.id)["basis_revision"].clone();
        self.submit(
            &format!("adopt-{pack_id}"),
            ACTOR,
            json!({"op":"method_pack.adopt","payload":{
                "artifact_ref": {"id": item["artifact_id"], "revision": 1}, "pack_id": pack_id,
                "version": "1.0.0", "digest": digest, "reason": "Example pack for this case."}}),
            json!([{"id": self.id, "revision": revision}]),
            0,
        );
        digest
    }

    fn observe(&self, key: &str, extensions: Value, exit: i32) -> Value {
        let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":self.attachment["digest"],
            "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
        self.submit(
            key,
            ACTOR,
            json!({"op":"observation.add","extensions":extensions,"payload":{
                "value":{"kind":"text","statement":"The archive search returned nothing."},
                "basis":{"kind":"source","artifact_ref":{"id":self.attachment["derivative_id"],"revision":1},
                    "selector":selector,"text_quote":{"exact":"Reading 12.5 units"}},
                "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]}}),
            json!([]),
            exit,
        )
    }

    fn context() -> Value {
        json!({
            "saw_original_conclusion":{"state":"known","value":true},
            "gathered_new_evidence":{"state":"known","value":false},
            "method_or_source_changes":[],"activity":"Read the recorded basis.",
            "dependencies":"shared","dependence_basis":"Same recorded data."
        })
    }
}

fn start(root: &Path, vault: bool) -> Case<'_> {
    let init: &[&str] = if vault {
        &["init", ".", "--json"]
    } else {
        &["init", ".", "--no-vault", "--json"]
    };
    run(root, init, None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("reading.txt"), "Reading 12.5 units\n").unwrap();
    let started = run(
        root,
        &[
            "start",
            "Which option should be chosen?",
            "--attach",
            "reading.txt",
            "--command-id",
            "start",
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        0,
    );
    Case {
        root,
        id: started["created"][0]["id"].as_str().unwrap().to_owned(),
        attachment: started["items"][0].clone(),
    }
}

#[test]
fn all_three_packs_load_with_the_unchanged_core_and_none_is_a_default() {
    for (id, text) in [
        ("source-research", SOURCE_RESEARCH),
        ("decision-analysis", DECISION_ANALYSIS),
        ("security-research", SECURITY_RESEARCH),
    ] {
        let loaded = load(text.as_bytes()).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        assert_eq!(loaded.id(), id);
        assert!(!loaded.pack().cards.is_empty());
    }
}

#[test]
fn an_unpinned_case_shows_no_example_pack_content() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path(), true);
    // Core guidance, the case view and the listing name only the built-in pack.
    let listing = output(
        case.root,
        &["--case", &case.id, "method", "list", "--json"],
        None,
        0,
    );
    let shown = output(
        case.root,
        &[
            "--case", &case.id, "show", &case.id, "--depth", "full", "--json",
        ],
        None,
        0,
    );
    let brief = output(case.root, &["--case", &case.id, "brief", "--json"], None, 0);
    let next = output(case.root, &["--case", &case.id, "next", "--json"], None, 0);
    for text in [&listing, &shown, &brief, &next] {
        let lower = text.to_lowercase();
        for word in [
            "security-research",
            "source-research",
            "decision-analysis",
            "security",
        ] {
            assert!(!lower.contains(word), "{word} in\n{text}");
        }
    }
    // The rendered vault, including each case Overview, carries none of it.
    output(case.root, &["vault", "render", "--json"], None, 0);
    let mut stack = vec![case.root.join("vault")];
    let mut files = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = fs::read_to_string(&path) {
                files += 1;
                assert!(!text.to_lowercase().contains("security"), "{path:?}");
            }
        }
    }
    assert!(files > 0);
    // An unadopted pack is not available as guidance even when the file is present.
    fs::write(case.root.join("security-research.toml"), SECURITY_RESEARCH).unwrap();
    output(
        case.root,
        &["--case", &case.id, "guide", "claim-scope", "--json"],
        None,
        3,
    );
}

#[test]
fn source_research_pins_and_checks_its_search_record() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path(), false);
    case.adopt("source-research", SOURCE_RESEARCH);
    for card in ["source-quality", "independence", "negative-result"] {
        let guide = run(
            case.root,
            &[
                "--case",
                &case.id,
                "guide",
                card,
                "--pack",
                "source-research",
                "--json",
            ],
            None,
            0,
        );
        assert_eq!(guide["guide"]["pack"]["pack_id"], "source-research");
    }
    let searches = json!({"source-research": {"searches": [
        {"where": "Regional archive index", "terms": "ledger 1890", "period": "1880-1900", "results": 0}]}});
    let accepted = case.observe("observe", searches.clone(), 0);
    let id = accepted["created"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(case.show(&id)["record"]["header"]["extensions"], searches);
    let refused = case.observe(
        "observe-bad",
        json!({"source-research": {"searches": [{"where": "Index"}]}}),
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
}

#[test]
fn decision_analysis_requires_a_reopen_trigger_and_disclosed_preferences_at_accept() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path(), false);
    let digest = case.adopt("decision-analysis", DECISION_ANALYSIS);
    for card in ["criteria", "compare", "sensitivity", "premortem"] {
        run(
            case.root,
            &[
                "--case",
                &case.id,
                "guide",
                card,
                "--pack",
                "decision-analysis",
                "--json",
            ],
            None,
            0,
        );
    }
    let question = case.one(
        "question",
        "question.create",
        json!({"importance":"normal","wording":"Which setup should be reused?"}),
    );
    let option = case.one("option", "option.create", json!({
        "question_ref":question,"description":"Reuse the setup","dependencies":[],"tradeoff_notes":[]}));
    let option = case.one(
        "eligible",
        "option.disposition",
        json!({"option_ref":option,"lifecycle":"eligible","reason":"Feasible"}),
    );
    let payload = json!({
        "question_ref":question,"choice":{"kind":"select","option_ref":option},
        "compared_alternatives":[option],"comparison_refs":[],"hard_conditions":[],
        "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Reuse the setup."],
        "reopen_trigger":"A reading disagrees.","expected_outcome":null,"reversal_conditions":[]
    });
    // Proposing is not gated; the preferences are declared as an extension.
    let proposed = case.submit(
        "propose-bare",
        ACTOR,
        json!({"op":"decision.propose","payload":payload}),
        json!([]),
        0,
    );
    let bare =
        json!({"id":proposed["created"][0]["id"],"revision":proposed["created"][0]["revision"]});
    let review_basis = case.show(bare["id"].as_str().unwrap())["review_basis"].clone();
    let review = case.one("review", "review.submit", json!({
        "kind":"validation","subject_ref":review_basis["subject_ref"],"basis_digest":review_basis["basis_digest"],
        "read_set":review_basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
        "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
        "context_isolation":"fresh_context_unblinded","context":Case::context()
    }));
    let refused = case.submit(
        "accept-bare",
        "agent:decider",
        json!({"op":"decision.accept","payload":{"decision_ref":bare,"review_ref":review,"acknowledgments":[]}}),
        json!([]),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    let blocker = &refused["error"]["expected"]["policy_blockers"][0];
    assert_eq!(blocker["pack_id"], "decision-analysis");
    assert_eq!(blocker["digest"], json!(digest));
    assert_eq!(
        blocker["unmet"]["missing_fields"],
        json!(["extensions.decision-analysis.subjective_preferences"])
    );

    // Disclosing the preferences on the proposal satisfies the rule.
    let disclosed = case.submit(
        "propose-disclosed",
        ACTOR,
        json!({"op":"decision.propose","payload":payload,
            "extensions":{"decision-analysis":{"subjective_preferences":"Prefers reuse over novelty; a judgment."}}}),
        json!([]),
        0,
    );
    let disclosed =
        json!({"id":disclosed["created"][0]["id"],"revision":disclosed["created"][0]["revision"]});
    let basis = case.show(disclosed["id"].as_str().unwrap())["review_basis"].clone();
    let review = case.one("review-disclosed", "review.submit", json!({
        "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
        "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
        "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
        "context_isolation":"fresh_context_unblinded","context":Case::context()
    }));
    let accepted = case.submit(
        "accept-disclosed",
        "agent:decider",
        json!({"op":"decision.accept","payload":{"decision_ref":disclosed,"review_ref":review,"acknowledgments":[]}}),
        json!([]),
        0,
    );
    assert_eq!(accepted["result"], "committed");
}

/// Words that would make guidance read as an instruction to act, a named
/// target or a claim of permission. The security pack must hold none.
const FORBIDDEN: &[&str] = &[
    "http",
    "://",
    "www.",
    "`",
    "$(",
    "&&",
    "||",
    "sudo",
    "curl",
    "wget",
    "nmap",
    "bash",
    "powershell",
    "exploit",
    "payload",
    "shellcode",
    "scan the",
    "run the",
    "execute",
    "launch",
    "attack",
    "penetration",
    "target",
    "authorized",
    "authorised",
    "authorization",
    "authorisation",
    "permitted",
    "approved",
    "you may",
    "go ahead",
    "is allowed",
    "is cleared",
    "cve-",
    "localhost",
    "127.0",
    "192.168",
    "10.0.",
];

#[test]
fn the_security_pack_holds_guidance_cards_and_no_executable_target_or_authority_content() {
    let loaded = load(SECURITY_RESEARCH.as_bytes()).unwrap();
    let pack = loaded.pack();
    // Cards only: nothing that could gate, extend, render or act.
    assert!(!pack.cards.is_empty());
    assert!(pack.properties.is_empty() && pack.policies.is_empty() && pack.guardrails.is_empty());
    assert!(
        pack.views.is_empty() && pack.reports.is_empty() && pack.observation_guidance.is_empty()
    );
    assert!(
        pack.cards
            .iter()
            .all(|card| card.operations.iter().all(|op| {
                op.ends_with(".add") // recording notes is the only thing a card may point at
            }))
    );
    let prose = SECURITY_RESEARCH
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    for word in FORBIDDEN {
        assert!(!prose.contains(word), "forbidden text {word:?}");
    }
    // Ten-dot numeric hosts or ports.
    let digits = prose
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == ':'))
        .filter(|part| part.matches('.').count() >= 3 || part.contains(':') && part.len() > 3);
    assert_eq!(digits.count(), 0, "address-like text");
}

#[test]
fn the_security_pack_pins_as_guidance_and_grants_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path(), false);
    let digest = case.adopt("security-research", SECURITY_RESEARCH);
    let guide = run(
        case.root,
        &[
            "--case",
            &case.id,
            "guide",
            "claim-scope",
            "--pack",
            "security-research",
            "--json",
        ],
        None,
        0,
    );
    assert_eq!(guide["guide"]["pack"]["digest"], json!(digest));
    // It adds no gate: a decision with no extra fields is not blocked by it.
    let shown = case.show(&case.id);
    let packs = shown["method_packs"].as_array().unwrap();
    assert_eq!(packs.len(), 2);
    assert_eq!(packs[1]["pack_id"], "security-research");
    // Its extension namespace declares no fields.
    let refused = case.observe(
        "observe-ext",
        json!({"security-research": {"anything": 1}}),
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
}
