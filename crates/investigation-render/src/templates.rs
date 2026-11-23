//! Note bodies. Pure.
//!
//! Record notes use the shared skeleton in [`crate::note`]: flat frontmatter,
//! display title, projection banner, basis table and the fixed sections.
//! Per-type templates extend [`record_note`] with their own sections. The case
//! Overview and `Start Here.md` are still deliberately small; their full
//! content is separate work behind the same functions.
//!
//! Authored text reaches these notes only through [`crate::text`] and
//! [`crate::yaml`], and only for records the view policy admits.

use std::fmt::Write as _;

use investigation_core::{identity::Id, state::Record};

pub use crate::note::BANNER;
use crate::{
    Generation, RENDERER_VERSION, RenderError, VerifiedSnapshot, ViewPolicy, layout,
    note::NoteContext,
};

fn heading(out: &mut String, title: &str) {
    let _ = write!(out, "# {title}\n\n{BANNER}\n");
}

fn case_link(case_id: Id) -> Result<String, RenderError> {
    Ok(layout::wikilink(
        &layout::case_overview(case_id)?,
        &case_id.to_string(),
    ))
}

/// One record's note: the shared skeleton with every fixed section.
pub fn record_note(notes: &NoteContext<'_>, record: &Record) -> Result<String, RenderError> {
    let mut out = String::new();
    notes.open_note(&mut out, record)?;
    notes.sections(&mut out, record)?;
    Ok(out)
}

pub fn case_overview(notes: &NoteContext<'_>) -> Result<String, RenderError> {
    let case = notes.case();
    let case_id = case.case_id();
    let state = case.state();
    let mut out = String::new();
    match state.current(case_id) {
        Some(record) => notes.open_note(&mut out, record)?,
        None => heading(&mut out, &case_id.to_string()),
    }
    out.push_str("\n## Records\n");
    let mut any = false;
    for &(kind, folder, _) in layout::RECORD_FOLDERS {
        let mut records = state
            .records()
            .filter(|record| record.header.id.kind() == kind)
            .peekable();
        if records.peek().is_none() {
            continue;
        }
        any = true;
        let _ = write!(out, "\n### {folder}\n\n");
        for record in records {
            let _ = write!(
                out,
                "- {} (revision {})",
                notes.link(record.header.id)?,
                record.header.revision.0
            );
            if !notes.visible(record) {
                out.push_str(", withheld");
            }
            out.push('\n');
        }
    }
    if !any {
        out.push_str("\nNo records beyond the case itself.\n");
    }
    Ok(out)
}

pub fn start_here(
    snapshot: &VerifiedSnapshot,
    policy: &ViewPolicy,
    _generation: &Generation,
) -> Result<String, RenderError> {
    let mut out = String::new();
    heading(&mut out, "Start Here");
    // The generation itself is the `inv_generation` marker property, added
    // by `crate::marker::stamp_plan`.
    let _ = writeln!(out, "- Renderer: `{RENDERER_VERSION}`");
    let _ = writeln!(out, "- View audience: `{}`", policy.audience.as_str());
    out.push_str("\n## Cases\n\n");
    let mut any = false;
    for case in snapshot.cases() {
        any = true;
        let head = case.head();
        let _ = writeln!(
            out,
            "- {}: sequence {}, head `{}`",
            case_link(case.case_id())?,
            head.sequence,
            head.head_hash
        );
    }
    if !any {
        out.push_str("No cases recorded yet.\n");
    }
    out.push_str(
        "\n## Human-owned folders\n\n\
         `Notes/`, `Boards/` and `Inbox/` belong to you. Rendering never writes into them.\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use investigation_core::{
        identity::Kind,
        state::{AcceptedTransaction, State},
        wire::{RecordChange, Snapshot},
    };
    use serde_json::{Value, json};

    use crate::{Audience, CaseRenderer, RenderPlan, VaultRenderer, note::BANNER};

    use super::*;

    const HOSTILE: &str = "Title ]] | [[Other]] <script>alert(1)</script> ![[secret.png]] \
        ![x](https://example.invalid/x.png) \u{202e}rtl #tag $math$ %%c%% ==hl== `code`\n---\ninv_id: forged\n---";
    const TIME: &str = "2026-11-21T22:00:00.000Z";
    const LATER: &str = "2026-11-22T08:30:00.000Z";

    fn id(kind: Kind, n: u64) -> Id {
        format!("{}_018d87fc-5234-7000-8000-{n:012}", kind.prefix())
            .parse()
            .unwrap()
    }

    fn reference(id: Id, revision: u64) -> Value {
        json!({"id":id, "revision":revision})
    }

    fn record(id: Id, case: Id, sequence: u64, revision: u64, level: &str, body: Value) -> Record {
        let recorded = if revision == 1 { TIME } else { LATER };
        serde_json::from_value(json!({
            "header":{
                "id":id, "case_id":case, "schema_version":"inv.record.v0-alpha",
                "created_by":"agent:render-test", "actor_descriptor_self_reported":null,
                "created_at":TIME, "recorded_at":recorded, "recorded_by":"agent:render-test",
                "observed_at":null, "acquired_at":null, "acceptance_sequence":sequence,
                "revision":revision, "history":(1..revision).collect::<Vec<_>>(),
                "classification":{"level":level, "reason":null, "tags":[]},
                "provenance":{"references":[]}, "extensions":{}
            },
            "body":body
        }))
        .unwrap()
    }

    /// A body whose every title-like field carries `title`.
    fn hostile(title: &str) -> Value {
        json!({
            "title":title, "wording":title, "statement":title, "claim":title,
            "description":title, "summary":title, "applicability":title,
            "value":{"kind":"text", "statement":title},
            "content":{"text":title}, "status":"open"
        })
    }

    fn apply(state: &mut State, case: Id, sequence: u64, records: Vec<Record>) {
        let (created, updated): (Vec<_>, Vec<_>) = records
            .iter()
            .map(|record| RecordChange {
                client_ref: None,
                id: record.header.id,
                revision: record.header.revision.0,
            })
            .partition(|change| change.revision == 1);
        state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records,
                created,
                updated,
                warnings: Vec::new(),
            })
            .unwrap();
    }

    struct Fixture {
        case: Id,
        question: Id,
        hypothesis: Id,
        secret: Id,
        annotation: Id,
        state: State,
    }

    /// One case with a record of every kind that has a note, a restricted
    /// question and an annotation that inherits its classification.
    fn fixture() -> Fixture {
        let case = id(Kind::Case, 1);
        let question = id(Kind::Question, 2);
        let hypothesis = id(Kind::Hypothesis, 3);
        let secret = id(Kind::Question, 4);
        let annotation = id(Kind::Annotation, 5);
        let artifact = id(Kind::Artifact, 6);
        let digest = format!("sha256:{}", "ab".repeat(32));
        let mut records = vec![record(case, case, 1, 1, "internal", hostile(HOSTILE))];
        for (n, &(kind, _, _)) in layout::RECORD_FOLDERS.iter().enumerate() {
            let this = match kind {
                Kind::Question => question,
                Kind::Hypothesis => hypothesis,
                Kind::Annotation => annotation,
                Kind::Artifact => artifact,
                other => id(other, 100 + n as u64),
            };
            let mut body = hostile(HOSTILE);
            match kind {
                Kind::Hypothesis => body["question_ref"] = reference(question, 1),
                // An artifact without a readable sensitivity label fails closed.
                Kind::Artifact => body["sensitivity"] = json!("internal"),
                Kind::Annotation => {
                    body["subject_refs"] = json!([reference(secret, 1)]);
                    body["content"]["text"] = json!("SECRET-ANNOTATION");
                }
                Kind::Observation => {
                    body["basis"] = json!({
                        "kind":"source", "artifact_ref":reference(artifact, 1),
                        "selector":{"schema_version":"inv.selector.v1", "artifact_digest":digest,
                            "selector":{"type":"text_quote", "exact":"pool `exhausted` <b>", "prefix":null, "suffix":null},
                            "validation":"verified"},
                        "text_quote":{"exact":"pool exhausted", "prefix":null, "suffix":null, "state":"verified"}
                    });
                }
                _ => {}
            }
            if kind == Kind::Question {
                records.push(record(
                    secret,
                    case,
                    1,
                    1,
                    "restricted",
                    json!({"wording":"TOP-SECRET-WORDING", "status":"open"}),
                ));
            }
            records.push(record(this, case, 1, 1, "internal", body));
        }
        let mut state = State::default();
        apply(&mut state, case, 1, records);
        Fixture {
            case,
            question,
            hypothesis,
            secret,
            annotation,
            state,
        }
    }

    fn render_state(state: &State, policy: &ViewPolicy) -> RenderPlan {
        let head = Snapshot {
            sequence: state.sequence(),
            head_hash: serde_json::from_value(json!(format!("sha256:{}", "cd".repeat(32))))
                .unwrap(),
        };
        let snapshot =
            VerifiedSnapshot::from_states(id(Kind::Workspace, 9), vec![(head, state.clone())]);
        VaultRenderer.render(&snapshot, policy).unwrap()
    }

    fn text(plan: &RenderPlan, path: &str) -> String {
        String::from_utf8(plan.entry(path).unwrap().content().to_vec()).unwrap()
    }

    fn note_path(case: Id, id: Id) -> String {
        layout::record_note(case, id).unwrap().unwrap().to_string()
    }

    fn frontmatter(note: &str) -> yaml_serde::Mapping {
        let rest = note
            .strip_prefix("---\n")
            .expect("note starts with frontmatter");
        let end = rest.find("\n---\n").expect("frontmatter ends");
        yaml_serde::from_str(&rest[..=end]).expect("frontmatter parses as YAML")
    }

    /// Wikilink targets, with the table-cell pipe escape removed.
    fn link_targets(note: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = note;
        while let Some(start) = rest.find("[[") {
            let after = &rest[start + 2..];
            let end = after.find("]]").expect("closed link");
            let inner = &after[..end];
            let target = inner.split('|').next().unwrap().trim_end_matches('\\');
            found.push(target.to_owned());
            rest = &after[end + 2..];
        }
        found
    }

    #[test]
    fn every_record_type_has_flat_frontmatter_banner_basis_and_inert_text() {
        let fixture = fixture();
        let plan = render_state(&fixture.state, &ViewPolicy::default());
        let generation = plan.generation().id().to_owned();
        let mut kinds = BTreeSet::new();
        for entry in plan.entries() {
            let note = std::str::from_utf8(entry.content()).unwrap();
            assert!(note.contains(BANNER), "{}", entry.path());
            // A case note names its case head, never the workspace
            // generation; only Start Here names the generation.
            let case_note = entry.path().as_str().starts_with("Cases/");
            assert_eq!(!case_note, note.contains(&generation), "{}", entry.path());
            assert!(crate::NoteMarker::read(note.as_bytes()).is_some());
            assert!(!note.chars().any(crate::text::is_invisible_format));
            for active in [
                "<script>",
                "[[Other",
                "![[",
                "](https",
                "\n---\ninv_id: forged",
                "#tag",
                "$math$",
                "%%c%%",
                "==hl==",
                "TOP-SECRET",
                "SECRET-ANNOTATION",
            ] {
                // A backslash before the first character makes the construct literal.
                let unescaped = note
                    .match_indices(active)
                    .any(|(at, _)| !note[..at].ends_with('\\'));
                assert!(!unescaped, "{active:?} in {}:\n{note}", entry.path());
            }
            for target in link_targets(note) {
                assert!(
                    target.starts_with("Cases/") && plan.entry(&format!("{target}.md")).is_some(),
                    "{target} in {} does not resolve",
                    entry.path()
                );
            }
            let Some(projected) = entry.projects() else {
                continue;
            };
            let record = fixture.state.current(projected).unwrap();
            let map = frontmatter(note);
            for (key, value) in &map {
                let flat = match value {
                    yaml_serde::Value::Sequence(items) => items.iter().all(|item| item.is_string()),
                    yaml_serde::Value::String(_)
                    | yaml_serde::Value::Number(_)
                    | yaml_serde::Value::Bool(_) => true,
                    _ => false,
                };
                assert!(flat, "{key:?} is not flat in {}", entry.path());
                let key = key.as_str().unwrap();
                assert!(key.starts_with("inv_") || key == "aliases" || key == "tags");
            }
            assert_eq!(map["inv_id"], projected.to_string().as_str());
            assert_eq!(map["inv_kind"], layout::kind_name(projected.kind()));
            assert_eq!(map["inv_case_id"], fixture.case.to_string().as_str());
            assert_eq!(
                map["inv_case"],
                format!("[[Cases/{}/Overview]]", fixture.case).as_str()
            );
            assert_eq!(map["inv_revision"], yaml_serde::Value::Number(1.into()));
            assert_eq!(
                map["inv_render_sequence"],
                yaml_serde::Value::Number(1.into())
            );
            assert_eq!(map["inv_schema"], "inv.vault.v1");
            assert!(!map.contains_key("inv_generation"));
            assert_eq!(
                map["inv_snapshot"],
                format!("sha256:{}", "cd".repeat(32)).as_str()
            );
            assert_eq!(
                map["tags"][0],
                format!("investigation/{}", layout::kind_name(projected.kind())).as_str()
            );
            assert!(
                note.contains("| Revision | 1 |"),
                "basis revision in {}",
                entry.path()
            );
            let withheld = [fixture.secret, fixture.annotation].contains(&projected);
            assert_eq!(
                withheld,
                map.contains_key("inv_withheld"),
                "{}:\n{note}",
                entry.path()
            );
            if withheld {
                assert_eq!(map["inv_withheld"], true);
                assert!(map["aliases"].as_sequence().unwrap().is_empty());
                assert!(!map.contains_key("inv_status") && !map.contains_key("inv_updated"));
                assert!(note.contains(&format!("# {projected}\n")));
            } else {
                assert_eq!(map["inv_status"], "open");
                assert_eq!(map["inv_updated"], TIME);
                let alias = map["aliases"][0].as_str().unwrap();
                assert!(
                    alias.starts_with("Title )) \u{a6} ((Other)) \u{2039}script\u{203a}alert(1)"),
                    "{alias}"
                );
                assert!(note.contains(&format!("\n# {alias}\n")));
            }
            if projected.kind() != Kind::Case {
                let headings: Vec<&str> = note
                    .lines()
                    .filter(|line| line.starts_with("## "))
                    .collect();
                assert_eq!(
                    headings,
                    [
                        "## Summary",
                        "## Typed links",
                        "## Evidence selectors",
                        "## Next action or blocker"
                    ]
                );
            }
            kinds.insert(record.header.id.kind());
        }
        let expected: BTreeSet<Kind> = layout::RECORD_FOLDERS
            .iter()
            .map(|&(kind, _, _)| kind)
            .chain([Kind::Case])
            .collect();
        assert_eq!(kinds, expected, "a note for every record type");

        // Typed links in both directions, with the field as the reason; the
        // restricted question is never named by the annotation that cites it.
        let question = text(&plan, &note_path(fixture.case, fixture.question));
        let hypothesis = text(&plan, &note_path(fixture.case, fixture.hypothesis));
        let hypothesis_target = note_path(fixture.case, fixture.hypothesis);
        let hypothesis_target = hypothesis_target.strip_suffix(".md").unwrap();
        assert!(question.contains(&format!(
            "- [[{hypothesis_target}|Title )) \u{a6} ((Other))"
        )));
        assert!(question.contains("through `question_ref`, citing revision 1"));
        assert!(
            hypothesis.contains(
                "through `question_ref`, citing revision 1; affected by changes to it only"
            )
        );
        let overview = text(&plan, &format!("Cases/{}/Overview.md", fixture.case));
        assert!(overview.contains(&format!(
            "{}|{}]] (revision 1), withheld",
            fixture.secret, fixture.secret
        )));

        // Exact selectors with a stable anchor; the excerpt stays in a code span.
        let observation = plan
            .entries()
            .iter()
            .find(|entry| {
                entry
                    .projects()
                    .is_some_and(|id| id.kind() == Kind::Observation)
            })
            .unwrap();
        let observation = std::str::from_utf8(observation.content()).unwrap();
        assert!(
            observation.contains("- `basis.selector`: artifact `sha256:abab"),
            "{observation}"
        );
        assert!(observation.contains("``{\"type\":\"text_quote\",\"exact\":\"pool `exhausted` <b>\",\"prefix\":null,\"suffix\":null}`` ^inv-sel-"));
    }

    #[test]
    fn a_title_change_keeps_files_and_links() {
        let mut fixture = fixture();
        let before = render_state(&fixture.state, &ViewPolicy::default());
        let mut body = hostile("Renamed: pool exhaustion | ]] precedes timeouts");
        body["question_ref"] = reference(fixture.question, 1);
        let revised = record(fixture.hypothesis, fixture.case, 2, 2, "internal", body);
        apply(&mut fixture.state, fixture.case, 2, vec![revised]);
        let after = render_state(&fixture.state, &ViewPolicy::default());

        let paths = |plan: &RenderPlan| -> Vec<String> {
            plan.entries()
                .iter()
                .map(|entry| entry.path().to_string())
                .collect()
        };
        assert_eq!(
            paths(&before),
            paths(&after),
            "no file renamed, added or lost"
        );
        let path = note_path(fixture.case, fixture.hypothesis);
        let note = text(&after, &path);
        assert!(note.contains("\n# Renamed: pool exhaustion \u{a6} )) precedes timeouts\n"));
        assert_eq!(
            frontmatter(&note)["aliases"][0],
            "Renamed: pool exhaustion \u{a6} )) precedes timeouts"
        );
        assert!(note.contains("| Revision | 2 (earlier: 1) |"));
        let question = text(&after, &note_path(fixture.case, fixture.question));
        let target = path.strip_suffix(".md").unwrap();
        assert!(question.contains(&format!(
            "[[{target}|Renamed: pool exhaustion \u{a6} )) precedes timeouts]]"
        )));
        for entry in after.entries() {
            for target in link_targets(std::str::from_utf8(entry.content()).unwrap()) {
                assert!(after.entry(&format!("{target}.md")).is_some(), "{target}");
            }
        }
    }

    #[test]
    fn a_narrower_audience_withholds_internal_records_but_keeps_identity() {
        let fixture = fixture();
        let plan = render_state(&fixture.state, &ViewPolicy::for_audience(Audience::Public));
        for entry in plan.entries() {
            let note = std::str::from_utf8(entry.content()).unwrap();
            assert!(!note.contains("((Other))"), "{}", entry.path());
            if let Some(projected) = entry.projects() {
                let map = frontmatter(note);
                assert_eq!(map["inv_withheld"], true);
                assert_eq!(map["inv_id"], projected.to_string().as_str());
                assert!(
                    note.contains("Content withheld by the view policy for the `public` audience.")
                );
            }
        }
        let restricted = render_state(
            &fixture.state,
            &ViewPolicy::for_audience(Audience::Restricted),
        );
        let secret = text(&restricted, &note_path(fixture.case, fixture.secret));
        assert!(secret.contains("\n# TOP-SECRET-WORDING\n"));
    }
}
