//! Evidence, artifact and observation notes show inspectable provenance.
//!
//! One case holds a restricted original, a redacted derivative of it and that
//! derivative's text derivative, both cleared by reviewed downgrades, a
//! verified quotation from the text derivative, a quotation
//! from the restricted original, and a second internal acquisition with a
//! source assessment and a partial withdrawal. The records are made through
//! the store's real ingest and record operations, so the rendered bodies are
//! the ones operations write.

use std::{fs, path::Path};

use investigation_core::{
    identity::{Id, Kind},
    wire::Receipt,
};
use investigation_render::{Audience, RenderPlan, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace, store::ingest::IngestReceipt};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use serde_json::{Value, json};

const ACTOR: &str = "agent:evidence-render-test";
/// Restricted bytes: they must never reach a note at the internal audience.
const SECRET: &str = "BLUEBIRD-SECRET";
const RESTRICTED_TEXT: &str = "Codename BLUEBIRD-SECRET ships first.\nThe queue limit is 25 ms.\n";
/// The quoted line of the derivative, hostile on purpose.
const QUOTE: &str = "The queue limit is 25 ms. See [[Other]] <script>x</script> #tag";
const UNQUOTED: &str = "Codename [removed] ships first.";

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, op: &str, payload: Value) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v0-alpha", "command_id":key, "case_id":case_id,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":op, "payload":payload}]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"))
}

fn ingest(store: &mut Store, manifest: &Value) -> IngestReceipt {
    store
        .ingest(&serde_json::to_vec(manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{failure:?}"))
}

fn manifest(root: &Path, key: &str, case: Id, contents: &str, level: &str) -> Value {
    let path = root.join(format!("{key}.txt"));
    fs::write(&path, contents).unwrap();
    json!({
        "schema_version":"inv.ingest.v1", "command_id":key, "actor":ACTOR, "case_id":case,
        "source":{"title":key, "format":"text/plain",
            "verification_limits":["Captured from a mirror ]] | [[Other]] <b>x</b>"]},
        "paths":[path], "classification":level, "extract_text":true
    })
}

fn selector(digest: &Value, exact: &str) -> Value {
    json!({"schema_version":"inv.selector.v1", "artifact_digest":digest,
        "selector":{"type":"text_quote", "exact":exact, "prefix":null, "suffix":null}})
}

fn lines(digest: &Value, start: u64, end: u64) -> Value {
    json!({"schema_version":"inv.selector.v1", "artifact_digest":digest,
        "selector":{"type":"text_lines", "start_line":start, "end_line":end,
            "encoding":"utf-8", "parser_version":"inv.text.utf8.v1"}})
}

fn observe(
    store: &mut Store,
    key: &str,
    case: Id,
    artifact: Id,
    digest: &Value,
    (start, end): (u64, u64),
    exact: &str,
) -> Id {
    submit(
        store,
        key,
        Some(case),
        "observation.add",
        json!({
            "value":{"kind":"text", "statement":format!("Observed: {key}")},
            "basis":{"kind":"source", "artifact_ref":{"id":artifact, "revision":1},
                "selector":lines(digest, start, end), "text_quote":{"exact":exact}},
            "validity":"valid", "quality":[], "limitations":["Single capture ==hl== %%c%%"],
            "evidence_kind":"observational", "variables_changed":[]
        }),
    )
    .created[0]
        .id
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    secret_source: Id,
    original: Id,
    original_digest: String,
    derivative: Id,
    text: Id,
    quoted: Id,
    secret_quote: Id,
    spec_source: Id,
    spec: Id,
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
    let case = submit(
        &mut store,
        "case",
        None,
        "case.create",
        json!({"title":"Which limit applies?", "mode":"record"}),
    )
    .created[0]
        .id;

    let secret = ingest(
        &mut store,
        &manifest(&root, "plan", case, RESTRICTED_TEXT, "restricted"),
    );
    let item = &secret.items[0];
    let (secret_source, original) = (item.source_id, item.artifact_id);
    let original_text = item.derivative_id.unwrap();
    let original_digest = json!(item.digest);

    let redacted = format!("{UNQUOTED}\n{QUOTE}\n");
    let mut derive = manifest(&root, "redact", case, &redacted, "internal");
    derive["derived_from"] = json!({
        "inputs":[{"artifact_ref":{"id":original, "revision":1}, "digest":original_digest}],
        "transformation":"example-org.redact", "version":"2",
        "selectors":[selector(&original_digest, SECRET)],
        "redaction_decisions":[{"scope":"Codenames", "action":"removed",
            "rationale":"Unannounced plan name [[Other]]."}],
        "review":{"method":"Read the whole output against the original.", "coverage":"whole_output"}
    });
    let derived = ingest(&mut store, &derive);
    let derivative = derived.items[0].artifact_id;
    let text = derived.items[0].derivative_id.unwrap();
    let derivative_digest = json!(derived.items[0].digest);
    submit(
        &mut store,
        "downgrade",
        Some(case),
        "classification.downgrade",
        json!({
            "subject_ref":{"id":derivative, "revision":1}, "level":"internal",
            "reason":"The codename was removed and the result reviewed.",
            "transformation":{
                "inputs":[{"input_ref":{"id":original, "revision":1}, "digest":original_digest},
                    {"input_ref":{"id":secret_source, "revision":1}, "digest":null}],
                "transformation":"example-org.redact", "version":"2",
                "redaction_decisions":[{"scope":"Codenames", "action":"removed",
                    "rationale":"Not needed to read the limit."}],
                "output_digest":derivative_digest
            },
            "review":{"method":"Read the whole output.", "coverage":"whole_output",
                "examined":[{"id":derivative, "revision":1}]}
        }),
    );
    // Its text derivative still names the restricted acquisition.
    submit(
        &mut store,
        "downgrade-text",
        Some(case),
        "classification.downgrade",
        json!({
            "subject_ref":{"id":text, "revision":1}, "level":"internal",
            "reason":"Decoded text of the reviewed redaction.",
            "transformation":{
                "inputs":[{"input_ref":{"id":derivative, "revision":1}, "digest":derivative_digest},
                    {"input_ref":{"id":secret_source, "revision":1}, "digest":null}],
                "transformation":"decode_utf8", "version":"inv.text.utf8.v1",
                "redaction_decisions":[{"scope":"Whole text", "action":"retained",
                    "rationale":"Same bytes as the reviewed redaction."}],
                "output_digest":derivative_digest
            },
            "review":{"method":"Read the whole output.", "coverage":"whole_output",
                "examined":[{"id":text, "revision":1}]}
        }),
    );
    let quoted = observe(
        &mut store,
        "quoted",
        case,
        text,
        &derivative_digest,
        (2, 2),
        QUOTE,
    );
    let secret_quote = observe(
        &mut store,
        "secret-quote",
        case,
        original_text,
        &original_digest,
        (1, 1),
        SECRET,
    );

    let spec = ingest(
        &mut store,
        &manifest(&root, "spec", case, "Limit 30 ms\n", "internal"),
    );
    let (spec_source, spec) = (spec.items[0].source_id, spec.items[0].artifact_id);
    submit(
        &mut store,
        "assess",
        Some(case),
        "source.assess",
        json!({
            "source_ref":{"id":spec_source, "revision":1},
            "authenticity":{"judgment":"authentic", "rationale":"From the publisher's archive."},
            "reliability":{"judgment":"mixed", "grounds":["track_record"],
                "rationale":"Earlier editions had errata <i>x</i>."},
            "limitations":["Only the retained edition was examined."],
            "evidence_refs":[]
        }),
    );
    submit(
        &mut store,
        "withdraw",
        Some(case),
        "artifact.withdraw",
        json!({
            "artifact_ref":{"id":spec, "revision":1}, "reason":"corrected_by_erratum",
            "scope":{"extent":"partial", "statement":"The stated limit."},
            "rationale":"An erratum replaces the limit ]] [[Other]].", "evidence_refs":[]
        }),
    );
    Fixture {
        _directory: directory,
        store,
        case,
        secret_source,
        original,
        original_digest: original_digest.as_str().unwrap().to_owned(),
        derivative,
        text,
        quoted,
        secret_quote,
        spec_source,
        spec,
    }
}

fn plan(fixture: &mut Fixture, audience: Audience) -> RenderPlan {
    let snapshot = VerifiedSnapshot::read(&mut fixture.store).unwrap();
    render(&snapshot, &ViewPolicy::for_audience(audience)).unwrap()
}

fn path(case: Id, id: Id) -> String {
    layout::record_note(case, id).unwrap().unwrap().to_string()
}

fn note(plan: &RenderPlan, case: Id, id: Id) -> String {
    String::from_utf8(plan.entry(&path(case, id)).unwrap().content().to_vec()).unwrap()
}

fn property(note: &str, key: &str) -> Option<String> {
    let front = note.strip_prefix("---\n")?.split("\n---\n").next()?;
    front
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .map(|value| value.trim_matches('"').to_owned())
}

/// Every link a viewer would follow, as (target, block anchor). Parsed, so
/// link-like text inside code is not counted.
fn links(note: &str) -> Vec<(String, Option<String>)> {
    Parser::new_ext(note, options())
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                // A table cell escapes the label pipe; the parser keeps the backslash.
                let dest = dest_url.trim_end_matches('\\');
                Some(match dest.split_once("#^") {
                    Some((target, anchor)) => (target.to_owned(), Some(anchor.to_owned())),
                    None => (dest.to_owned(), None),
                })
            }
            _ => None,
        })
        .collect()
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_GFM
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

#[test]
fn restricted_original_derivative_and_verified_quote_render_with_provenance() {
    let mut fixture = fixture();
    let case = fixture.case;
    let plan = plan(&mut fixture, Audience::Internal);

    // Nothing restricted reaches any note: no restricted bytes, quote or
    // digest, and no retained bytes that no recorded quote names.
    for entry in plan.entries() {
        let text = std::str::from_utf8(entry.content()).unwrap();
        for leak in [SECRET, fixture.original_digest.as_str(), UNQUOTED] {
            assert!(!text.contains(leak), "{leak} in {}:\n{text}", entry.path());
        }
        // Every link, including block links, resolves to a planned note and,
        // for a block link, to a block anchor in it.
        for (target, anchor) in links(text) {
            let linked = plan
                .entry(&format!("{target}.md"))
                .unwrap_or_else(|| panic!("{target} in {} does not resolve", entry.path()));
            if let Some(anchor) = anchor {
                let linked = std::str::from_utf8(linked.content()).unwrap();
                assert!(
                    linked.contains(&format!(" ^{anchor}\n")),
                    "^{anchor} missing in {target}"
                );
            }
        }
        // Captured and authored text never becomes a link, HTML or a
        // fence with a source-provided language.
        for event in Parser::new_ext(text, options()) {
            match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    assert!(
                        dest_url.starts_with("Cases/"),
                        "{dest_url} in {}",
                        entry.path()
                    );
                }
                Event::Html(html) | Event::InlineHtml(html) => {
                    panic!("HTML {html:?} in {}", entry.path())
                }
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language))) => {
                    assert_eq!(&*language, "text", "{}", entry.path());
                }
                _ => {}
            }
        }
    }

    // The restricted original, its Source and the quote from it are
    // ID-only frames.
    for id in [
        fixture.original,
        fixture.secret_source,
        fixture.secret_quote,
    ] {
        let text = note(&plan, case, id);
        assert_eq!(
            property(&text, "inv_withheld").as_deref(),
            Some("true"),
            "{text}"
        );
        assert!(property(&text, "inv_artifact_role").is_none());
        assert!(
            text.contains("\n## Evidentiary use\n\nWithheld.\n"),
            "{text}"
        );
        assert!(!text.contains("[!quote]"));
    }

    // The derivative: role, integrity, classification basis, lineage to the
    // withheld original by ID only, the derivation record and its excerpts.
    let derivative = note(&plan, case, fixture.derivative);
    let original_target = path(case, fixture.original);
    let original_target = original_target.strip_suffix(".md").unwrap();
    for expected in [
        "| Role | `derivative`: the output of a transformation of other artifacts, never a new acquisition |",
        "| Availability | `retained` |",
        "| Integrity | `verified` |",
        "| Sensitivity | labelled `internal`; effective `internal`, lowered by the reviewed downgrade [[",
        "1 derivation step from the original acquisition.",
        &format!(
            "- [[{original_target}|{}]] revision 1 through `example-org.redact` version `2`, input digest withheld",
            fixture.original
        ),
        "Other artifacts from the same acquisition: views of the same captured bytes",
        "- Review: `whole_output` coverage: Read the whole output against the original\\.",
        "- Redaction: Codenames `removed`: Unannounced plan name \\[\\[Other\\]\\]\\.",
        "- Input portions used: 1 selector (`text_quote`)",
        "  - Exact selectors withheld: an input is withheld by the view policy.",
        "The retained bytes are not copied into the vault.",
    ] {
        assert!(
            derivative.contains(expected),
            "{expected}\n---\n{derivative}"
        );
    }
    assert_eq!(
        property(&derivative, "inv_artifact_role").as_deref(),
        Some("derivative")
    );
    assert_eq!(
        property(&derivative, "inv_lineage_depth").as_deref(),
        Some("1")
    );
    assert_eq!(
        property(&derivative, "inv_integrity").as_deref(),
        Some("verified")
    );
    let origin_line = derivative
        .lines()
        .find(|line| line.contains("retained as"))
        .unwrap();
    // The withheld origin's availability and integrity stay out.
    assert!(!origin_line.contains("availability"), "{origin_line}");

    // The text derivative traces through the redaction to the original.
    let text = note(&plan, case, fixture.text);
    let derivative_target = path(case, fixture.derivative);
    let derivative_target = derivative_target.strip_suffix(".md").unwrap();
    for expected in [
        "2 derivation steps from the original acquisition.",
        &format!(
            "- [[{derivative_target}|redact]] revision 1 through `decode_utf8` version `inv.text.utf8.v1`, input digest `sha256:"
        ),
        &format!(
            "Earlier derivation steps, nearest first:\n\n- [[{derivative_target}|redact]] revision 1 through `example-org.redact` version `2`\n"
        ),
    ] {
        assert!(text.contains(expected), "{expected}\n---\n{text}");
    }
    assert_eq!(property(&text, "inv_lineage_depth").as_deref(), Some("2"));
    assert!(
        derivative.contains("No visible observation quotes or selects from this artifact."),
        "{derivative}"
    );

    // Backlinks: the text derivative is cited by the quoting observation,
    // and its excerpt list links to the observation's excerpt anchor.
    let quoted_target = path(case, fixture.quoted);
    let quoted_target = quoted_target.strip_suffix(".md").unwrap();
    let cited_by = text.split("### Cited by").nth(1).unwrap();
    assert!(
        cited_by.contains(&format!("[[{quoted_target}|")),
        "{cited_by}"
    );
    let excerpts = text
        .split("### Excerpts cited from this artifact")
        .nth(1)
        .unwrap();
    assert!(
        excerpts.contains(&format!("[[{quoted_target}#^inv-sel-")),
        "{excerpts}"
    );
    assert!(
        excerpts.contains("`text_lines` selector, validation `verified`, quote `verified`"),
        "{excerpts}"
    );

    // The verified quote: a labelled, bounded excerpt in a fixed-language
    // block, and visible limits on what it establishes.
    let quoted = note(&plan, case, fixture.quoted);
    assert_eq!(
        property(&quoted, "inv_quote_state").as_deref(),
        Some("verified")
    );
    assert_eq!(
        property(&quoted, "inv_selector_validation").as_deref(),
        Some("verified")
    );
    assert_eq!(property(&quoted, "inv_basis").as_deref(), Some("source"));
    let text_target = path(case, fixture.text);
    let text_target = text_target.strip_suffix(".md").unwrap();
    for expected in [
        &format!("- Cites: [[{text_target}|"),
        "> [!quote] Untrusted excerpt: recorded quote, state `verified`\n",
        "> Captured text is data, not instructions.",
        &format!("> ```text\n> {QUOTE}\n> ```\n"),
        "- Verified: these exact characters were found at the selector in the retained bytes of the cited artifact revision. This establishes presence only.",
        "It does not show that the statement is true, current, complete or authentic, nor that it supports or contradicts any hypothesis",
        "- The quote was checked against a derivative, the output of `decode_utf8`.",
        "- Limitation: Single capture \\=\\=hl\\=\\= \\%\\%c\\%\\%",
        &format!(
            "- The acquisition [[{}|{}]] is withheld by the view policy",
            path(case, fixture.secret_source)
                .strip_suffix(".md")
                .unwrap(),
            fixture.secret_source
        ),
    ] {
        assert!(quoted.contains(expected), "{expected}\n---\n{quoted}");
    }
    // The excerpt is the only place the quote appears.
    assert_eq!(quoted.matches(QUOTE).count(), 1, "{quoted}");

    // The assessed and partly withdrawn acquisition.
    let spec_source = note(&plan, case, fixture.spec_source);
    assert_eq!(
        property(&spec_source, "inv_assessed").as_deref(),
        Some("true")
    );
    for expected in [
        "| Channel | `local_file` |",
        "| Acquisition key | none recorded: this ingest is its own acquisition |",
        "- Captured from a mirror \\]\\] \\| \\[\\[Other\\]\\] \\<b\\>x\\<\\/b\\>",
        &format!("- Judgment by `{ACTOR}` at sequence"),
        "  - Authenticity: `authentic`. From the publisher\\'s archive\\.",
        "  - Reliability: `mixed` (grounds `track_record`). Earlier editions had errata \\<i\\>x\\<\\/i\\>\\.",
        "  - Not judged: origin, currency.",
        "  - Limitation: Only the retained edition was examined\\.",
    ] {
        assert!(
            spec_source.contains(expected),
            "{expected}\n---\n{spec_source}"
        );
    }
    let spec = note(&plan, case, fixture.spec);
    assert_eq!(
        property(&spec, "inv_evidentiary_use").as_deref(),
        Some("partially_withdrawn")
    );
    assert_eq!(
        property(&spec, "inv_artifact_role").as_deref(),
        Some("original")
    );
    for expected in [
        "State: `partially_withdrawn`.",
        "withdrawn in part by `agent:evidence-render-test`",
        "reason `corrected_by_erratum`: An erratum replaces the limit \\]\\] \\[\\[Other\\]\\]\\.",
        "  - Withdrawn part: The stated limit\\.",
        "| Role | `original`: the bytes of one acquisition |",
        "No visible observation quotes or selects from this artifact.",
    ] {
        assert!(spec.contains(expected), "{expected}\n---\n{spec}");
    }
    let spec_target = path(case, fixture.spec);
    let spec_target = spec_target.strip_suffix(".md").unwrap();
    assert!(
        spec_source
            .split("### Cited by")
            .nth(1)
            .unwrap()
            .contains(&format!("[[{spec_target}|")),
        "{spec_source}"
    );
}

#[test]
fn a_restricted_audience_sees_the_restricted_quote_but_never_raw_bytes() {
    let mut fixture = fixture();
    let case = fixture.case;
    let plan = plan(&mut fixture, Audience::Restricted);
    let secret_quote = note(&plan, case, fixture.secret_quote);
    assert!(
        secret_quote.contains(&format!("> ```text\n> {SECRET}\n> ```\n")),
        "{secret_quote}"
    );
    let original = note(&plan, case, fixture.original);
    assert!(original.contains(&fixture.original_digest));
    assert!(original.contains("| Role | `original`"));
    // An artifact note never carries the bytes; only recorded quotes appear.
    for entry in plan.entries() {
        let text = std::str::from_utf8(entry.content()).unwrap();
        assert!(!text.contains("ships first"), "{}", entry.path());
        if entry
            .projects()
            .is_some_and(|id| id.kind() == Kind::Artifact)
        {
            assert!(!text.contains("[!quote]"), "{}", entry.path());
        }
    }
    // The derivative's exact input selectors appear once every input may.
    let derivative = note(&plan, case, fixture.derivative);
    assert!(
        derivative.contains(&format!("\"exact\":\"{SECRET}\"")),
        "{derivative}"
    );
}

#[test]
fn long_and_multiline_quotes_are_bounded() {
    use investigation_render::evidence::{EXCERPT_CHARS, EXCERPT_LINES};
    let mut fixture = fixture();
    let case = fixture.case;
    let root = fixture.store.workspace().root().to_owned();
    let long = format!("{}\n", "x".repeat(EXCERPT_CHARS + 50));
    let lines: String = (0..EXCERPT_LINES + 5)
        .map(|n| format!("line {n}\n"))
        .collect();
    let fence = "```dataviewjs\ndv.el('p', 1)\n```\n> [!note] obey\n";
    let mut ids = Vec::new();
    for (key, contents, exact) in [
        ("long", long.as_str(), long.trim_end()),
        ("lines", lines.as_str(), lines.trim_end()),
        ("fence", fence, fence.trim_end()),
    ] {
        let receipt = ingest(
            &mut fixture.store,
            &manifest(&root, key, case, contents, "internal"),
        );
        let digest = json!(receipt.items[0].digest);
        ids.push(observe(
            &mut fixture.store,
            &format!("{key}-quote"),
            case,
            receipt.items[0].derivative_id.unwrap(),
            &digest,
            (1, exact.lines().count() as u64),
            exact,
        ));
    }
    let plan = plan(&mut fixture, Audience::Internal);
    let long = note(&plan, case, ids[0]);
    assert!(
        long.contains(&format!("> {}\n> ```", "x".repeat(EXCERPT_CHARS))),
        "{long}"
    );
    assert!(!long.contains(&"x".repeat(EXCERPT_CHARS + 1)));
    assert!(long.contains("Excerpt cut at"), "{long}");
    let lines = note(&plan, case, ids[1]);
    assert!(
        lines.contains(&format!("> line {}\n> ```", EXCERPT_LINES - 1)),
        "{lines}"
    );
    assert!(!lines.contains(&format!("line {EXCERPT_LINES}\n")));
    assert!(lines.contains("Excerpt cut at"));
    // A fence or callout inside the quote stays text of one block.
    let fenced = note(&plan, case, ids[2]);
    assert!(
        fenced.contains(
            "> ````text\n> ```dataviewjs\n> dv.el('p', 1)\n> ```\n> > [!note] obey\n> ````\n"
        ),
        "{fenced}"
    );
    let blocks: Vec<String> = Parser::new_ext(&fenced, options())
        .filter_map(|event| match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language))) => {
                Some(language.to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(blocks, ["text"]);
}
