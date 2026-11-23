//! `Start Here.md`: the vault's opening note. Pure.
//!
//! A person who has never seen the tool opens this note first. It says what
//! the vault is, whether what they are looking at is complete, which folders
//! are theirs, and, per case, where the case stands and the exact command
//! that resumes it. It is plain Markdown with wikilinks: no embedded query,
//! no community plugin.
//!
//! The note is a function of the snapshot, the view policy and an explicit
//! [`EntryHealth`]. The plan renders it healthy; a working-vault render
//! publishes the variant that announces a partial render or recorded drift
//! (see `crate::entry`). Nothing here reads a clock, so the same inputs give
//! the same bytes.
//!
//! Bounds: at most [`MAX_CASES`] cases are described, in case ID order, with
//! a count of the rest; every per-case list shows at most [`MAX_ITEMS`]
//! entries and links to the case Overview for the rest. Lists show only
//! records the view policy admits, and a withheld case shows its ID only.

use std::fmt::Write as _;

use investigation_core::{
    PROGRAM_NAME,
    identity::{Id, Kind},
    state::Record,
};

use crate::{
    DriftKind, Generation, PlanPath, RENDERER_VERSION, RenderError, VerifiedSnapshot, ViewPolicy,
    evidence::prose,
    layout::{self, Location},
    note::{BANNER, NoteContext, text_at},
    records::{effective_word, lifecycle, overview, reviews::freshness_word},
    text::inline_code,
};

/// Cases described on the page; the rest are counted.
pub const MAX_CASES: usize = 25;
/// Entries per per-case list; the rest are counted and linked.
pub const MAX_ITEMS: usize = 5;
/// Drifted paths named on the page; the rest are counted.
pub const MAX_DRIFT_LISTED: usize = 10;

/// What the opening note says about the projection itself, beside the
/// snapshot. The default is a healthy vault: complete and without drift.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryHealth {
    /// The render of this generation stopped before it processed every plan
    /// entry, for this reason. `None` when every entry before this note was
    /// processed.
    pub partial: Option<String>,
    /// Owned paths whose files changed outside the renderer, in path order.
    pub drift: Vec<(PlanPath, DriftKind)>,
}

impl EntryHealth {
    pub const fn is_healthy(&self) -> bool {
        self.partial.is_none() && self.drift.is_empty()
    }
}

/// The whole note. `cases` holds a context for (at least) the first
/// [`MAX_CASES`] cases of `snapshot`, in case ID order.
pub fn note(
    snapshot: &VerifiedSnapshot,
    cases: &[NoteContext<'_>],
    policy: &ViewPolicy,
    generation: &Generation,
    health: &EntryHealth,
) -> Result<String, RenderError> {
    let mut out = String::new();
    // The generation is also the `inv_generation` property, added by
    // `crate::marker::stamp_plan`; Start Here is the only note carrying it.
    let _ = write!(
        out,
        "# Start Here\n\n{BANNER}\n\
         This vault is a read-only view of the Investigation workspace that contains it. The \
         workspace journal is the record; every note here is projected from one verified \
         snapshot of it, and nothing written in the vault changes it. The notes are plain \
         Markdown with wikilinks, and nothing here needs a community plugin.\n\n\
         - Renderer: {}\n- View audience: {}\n",
        inline_code(RENDERER_VERSION),
        inline_code(policy.audience.as_str()),
    );
    health_section(&mut out, generation, health);
    ownership_section(&mut out);
    resume_intro(&mut out);
    cases_section(&mut out, snapshot, cases)?;
    Ok(out)
}

fn health_section(out: &mut String, generation: &Generation, health: &EntryHealth) {
    let _ = write!(
        out,
        "\n## Projection health\n\n- Generation: {} (the `inv_generation` property of this \
         note)\n",
        inline_code(generation.id())
    );
    match &health.partial {
        None => out.push_str(
            "- State: complete. A render publishes this note after every case note, so the \
             notes it links to reflect the case heads listed below.\n",
        ),
        Some(reason) => {
            let _ = writeln!(
                out,
                "- State: partial. The render of this generation stopped before it \
                 processed every note: {}. Notes may mix generations; each case note names \
                 the case head it was rendered at in `inv_render_sequence` and \
                 `inv_snapshot`. The next render completes the vault, and `brief` keeps \
                 reading the workspace itself.",
                prose(reason)
            );
        }
    }
    if health.drift.is_empty() {
        out.push_str("- Drift: none identified when this note was written.\n");
    } else {
        let mut counts = Vec::new();
        for kind in DriftKind::ALL {
            let count = health.drift.iter().filter(|(_, k)| *k == kind).count();
            if count > 0 {
                counts.push(format!("{} {count}", kind.as_str()));
            }
        }
        let _ = writeln!(
            out,
            "- Drift: {} generated file{} changed outside the renderer ({}). Each is left \
             exactly as found and is no longer updated; the edit is not recorded. The version \
             the renderer would have written is kept in the workspace under \
             `local/conflicts/<generation>/`.",
            health.drift.len(),
            if health.drift.len() == 1 { "" } else { "s" },
            counts.join(", ")
        );
        for (path, kind) in health.drift.iter().take(MAX_DRIFT_LISTED) {
            let _ = writeln!(
                out,
                "  - {} ({})",
                inline_code(path.as_str()),
                kind.as_str()
            );
        }
        if health.drift.len() > MAX_DRIFT_LISTED {
            let _ = writeln!(
                out,
                "  - and {} more",
                health.drift.len() - MAX_DRIFT_LISTED
            );
        }
        out.push_str(
            "  - Resolve each with `vault reconcile`, the path and one disposition: \
             `preserve-as-note` moves your edit to `Notes/Reconciled/` and regenerates the \
             note, `propose-change` records a supported change you state, `discard` drops the \
             edit (kept under `local/conflicts/discarded/`), and `release-path` makes the file \
             yours and moves the note beside it. Nothing is read from the edit itself. \
             `vault conflicts` prints the exact command.\n",
        );
    }
    out.push_str(
        "- Each case note names the case head its exact content was rendered at, in \
         `inv_render_sequence` and `inv_snapshot`. A render rewrites only the notes whose \
         content changed, so a note that later commits left unchanged keeps an older \
         sequence than its case's below. While a render runs, notes can briefly mix \
         generations; once it completes, every note is one that render confirmed.\n",
    );
    // Run in the workspace folder; `vault_scenarios.rs` runs both verbatim.
    let _ = writeln!(
        out,
        "- Check it from the workspace folder: {} says whether this vault matches the \
         workspace's current snapshot and is complete, and {} lists each drifted file with \
         where its withheld version is kept. Neither changes anything.",
        inline_code(&format!("{PROGRAM_NAME} vault status --format json")),
        inline_code(&format!("{PROGRAM_NAME} vault conflicts --format json")),
    );
}

fn ownership_section(out: &mut String) {
    out.push_str("\n## Who owns what\n\n| Location | Owner and use |\n|---|---|\n");
    for area in layout::VAULT_AREAS {
        let locations: Vec<String> = area
            .locations
            .iter()
            .map(|location| match location {
                Location::File(name) => inline_code(name),
                Location::Folder(name) => inline_code(&format!("{name}/")),
                Location::HiddenFolders => "Hidden folders (names starting with `.`)".to_owned(),
            })
            .collect();
        let _ = writeln!(out, "| {} | {} |", locations.join(", "), area.guide);
    }
    out.push_str(
        "\nLinking to a generated note from your own note is fine. Changes to a case go through \
         `investigation` commands, never through edits to generated notes. Your notes are not \
         evidence: to rely on one, add it to a case with an `investigation` command, which keeps \
         that exact text by its hash, so a later edit to the note does not change what was \
         recorded.\n",
    );
}

fn resume_intro(out: &mut String) {
    out.push_str(
        "\n## Resume a case\n\n\
         Run the commands listed under each case in a terminal, in the workspace folder (the \
         folder that contains this vault and `investigation.toml`) or any folder inside it. \
         `brief` is the bounded handoff for a case; `frontier` lists every open gap. Both read \
         one verified snapshot of the workspace, so they stay correct even when this vault is \
         stale.\n",
    );
}

fn cases_section(
    out: &mut String,
    snapshot: &VerifiedSnapshot,
    cases: &[NoteContext<'_>],
) -> Result<(), RenderError> {
    out.push_str("\n## Cases\n\n");
    let total = snapshot.cases().count();
    match total {
        0 => {
            out.push_str("No cases recorded yet.\n");
            return Ok(());
        }
        1 => out.push_str("One case.\n"),
        _ if total <= MAX_CASES => {
            let _ = writeln!(out, "{total} cases, in case ID order.");
        }
        _ => {
            let _ = writeln!(
                out,
                "{total} cases. The first {MAX_CASES} in case ID order are described here; \
                 each of the other {} has its own Overview under `Cases/<case ID>/`.",
                total - MAX_CASES
            );
        }
    }
    for notes in cases.iter().take(MAX_CASES) {
        case_block(out, notes)?;
    }
    Ok(())
}

fn case_block(out: &mut String, notes: &NoteContext<'_>) -> Result<(), RenderError> {
    let case = notes.case();
    let case_id = case.case_id();
    let state = case.state();
    let record = state.current(case_id);
    let visible = record.is_some_and(|record| notes.visible(record));
    // The link shows the case title, or its ID when the title is withheld.
    let overview_link = notes.link(case_id)?;
    let head = case.head();
    let _ = write!(
        out,
        "\n- {overview_link}\n  - Case ID: {}\n\
         \x20 - Last committed snapshot: sequence {}, head {}\n",
        inline_code(&case_id.to_string()),
        head.sequence,
        inline_code(head.head_hash.as_str()),
    );
    match record {
        Some(record) if visible => {
            let answer = match overview::primary_outcome(state) {
                Some(outcome) if notes.visible(outcome) => {
                    let mut line = notes.link(outcome.header.id)?;
                    if let Some(word) = effective_word(notes, outcome) {
                        let _ = write!(line, ", effective {}", inline_code(&word));
                    }
                    line
                }
                Some(_) => "withheld".to_owned(),
                None => "no conclusion or decision recorded yet".to_owned(),
            };
            let _ = write!(
                out,
                "  - Current answer: {answer}\n  - Case freshness: {}\n  - Focus: {}\n",
                inline_code(overview::case_freshness(notes, record)),
                overview::focus(record, state).word(),
            );
            // The lists below show visible records only; say what they omit
            // so a withheld review or question never reads as none.
            let withheld = state
                .records()
                .filter(|record| notes.placeholder(record))
                .count();
            if withheld > 0 {
                let _ = writeln!(
                    out,
                    "  - Withheld by the view policy: {withheld} record{}, listed by ID in \
                     {overview_link}; the lists below leave them out",
                    if withheld == 1 { "" } else { "s" }
                );
            }
        }
        _ => out.push_str("  - Current answer: withheld\n"),
    }
    let lists: [(&str, Vec<String>); 4] = [
        ("Pending questions", pending_questions(notes)?),
        ("Outstanding reviews", outstanding_reviews(notes)?),
        (
            "Unresolved blindspot reviews",
            unresolved_blindspots(notes)?,
        ),
        ("Retrospectives", retrospectives(notes)?),
    ];
    for (label, items) in lists {
        bounded(out, label, &items, &overview_link);
    }
    let _ = writeln!(out, "  - Resume:");
    for command in ["brief", "frontier"] {
        let _ = writeln!(
            out,
            "    - {}",
            inline_code(&format!(
                "{PROGRAM_NAME} --case {case_id} {command} --format json"
            ))
        );
    }
    Ok(())
}

/// One labelled list: "none identified" when empty, otherwise at most
/// [`MAX_ITEMS`] entries and a link to the Overview for the rest.
fn bounded(out: &mut String, label: &str, items: &[String], overview_link: &str) {
    if items.is_empty() {
        let _ = writeln!(out, "  - {label}: none identified");
        return;
    }
    let _ = writeln!(out, "  - {label} ({}):", items.len());
    for item in items.iter().take(MAX_ITEMS) {
        let _ = writeln!(out, "    - {item}");
    }
    if items.len() > MAX_ITEMS {
        let _ = writeln!(
            out,
            "    - and {} more in {overview_link}",
            items.len() - MAX_ITEMS
        );
    }
}

/// Visible records of `kind` in this case, oldest first.
fn of_kind<'a>(notes: &NoteContext<'a>, kind: Kind) -> Vec<&'a Record> {
    let mut found: Vec<&Record> = notes
        .case()
        .state()
        .records()
        .filter(|record| record.header.id.kind() == kind && notes.visible(record))
        .collect();
    found.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    found
}

fn item(notes: &NoteContext<'_>, id: Id, detail: &str) -> Result<String, RenderError> {
    Ok(format!("{}: {detail}", notes.link(id)?))
}

/// Questions still `open` or `deferred`.
fn pending_questions(notes: &NoteContext<'_>) -> Result<Vec<String>, RenderError> {
    let mut items = Vec::new();
    for record in of_kind(notes, Kind::Question) {
        let Some(word @ ("open" | "deferred")) = text_at(&record.body, "disposition") else {
            continue;
        };
        items.push(item(notes, record.header.id, &inline_code(word))?);
    }
    Ok(items)
}

/// Conclusions marked ready for review, and submitted reviews that are no
/// longer current at this snapshot.
fn outstanding_reviews(notes: &NoteContext<'_>) -> Result<Vec<String>, RenderError> {
    let mut items = Vec::new();
    for record in of_kind(notes, Kind::Conclusion) {
        if lifecycle(record) == Some("review_ready") {
            items.push(item(
                notes,
                record.header.id,
                "`review_ready`, awaiting review",
            )?);
        }
    }
    for record in of_kind(notes, Kind::Review) {
        let word = freshness_word(notes, record);
        if matches!(word, "stale" | "disputed") {
            items.push(item(
                notes,
                record.header.id,
                &format!("review is {}", inline_code(word)),
            )?);
        }
    }
    Ok(items)
}

/// Blindspot reviews not superseded that reported gaps or were
/// inconclusive, or that are no longer current.
fn unresolved_blindspots(notes: &NoteContext<'_>) -> Result<Vec<String>, RenderError> {
    let mut items = Vec::new();
    for record in of_kind(notes, Kind::Review) {
        if text_at(&record.body, "kind") != Some("blindspot") {
            continue;
        }
        let freshness = freshness_word(notes, record);
        let verdict = text_at(&record.body, "verdict").unwrap_or("not recorded");
        let settled = verdict == "no_gap_identified_within_scope";
        if freshness == "superseded" || (settled && freshness == "current") {
            continue;
        }
        items.push(item(
            notes,
            record.header.id,
            &format!(
                "verdict {}, freshness {}",
                inline_code(verdict),
                inline_code(freshness)
            ),
        )?);
    }
    Ok(items)
}

/// Retrospectives not superseded, newest first.
fn retrospectives(notes: &NoteContext<'_>) -> Result<Vec<String>, RenderError> {
    let mut items = Vec::new();
    for record in of_kind(notes, Kind::Retrospective).into_iter().rev() {
        let freshness = freshness_word(notes, record);
        if freshness == "superseded" {
            continue;
        }
        items.push(item(
            notes,
            record.header.id,
            &format!("freshness {}", inline_code(freshness)),
        )?);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use investigation_core::{
        state::{AcceptedTransaction, State},
        wire::{RecordChange, Snapshot},
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::{Audience, CaseRenderer, RenderPlan, VaultRenderer};

    const TIME: &str = "2026-11-21T22:00:00.000Z";

    fn id(kind: Kind, case: u64, n: u64) -> Id {
        format!("{}_018d87fc-5234-7000-8000-{case:04}{n:08}", kind.prefix())
            .parse()
            .unwrap()
    }

    fn record(id: Id, case: Id, level: &str, body: Value) -> Record {
        serde_json::from_value(json!({
            "header":{
                "id":id, "case_id":case, "schema_version":"inv.record.v0-alpha",
                "created_by":"agent:start-here-test", "actor_descriptor_self_reported":null,
                "created_at":TIME, "recorded_at":TIME, "recorded_by":"agent:start-here-test",
                "observed_at":null, "acquired_at":null, "acceptance_sequence":1,
                "revision":1, "history":[],
                "classification":{"level":level, "reason":null, "tags":[]},
                "provenance":{"references":[]}, "extensions":{}
            },
            "body":body
        }))
        .unwrap()
    }

    /// Case `n` titled "Case n" at `level`, with `questions` open questions.
    fn case(n: u64, level: &str, questions: u64) -> (Snapshot, State) {
        let case = id(Kind::Case, n, 0);
        let mut records = vec![record(
            case,
            case,
            level,
            json!({"title":format!("Case {n} ]] [[x"), "mode":"explain", "status":"open"}),
        )];
        for q in 1..=questions {
            records.push(record(
                id(Kind::Question, n, q),
                case,
                "internal",
                json!({"wording":format!("Question {q}"), "disposition":"open"}),
            ));
        }
        records.push(record(
            id(Kind::Question, n, 99),
            case,
            "internal",
            json!({"wording":"Settled", "disposition":"answered"}),
        ));
        let created = records
            .iter()
            .map(|record| RecordChange {
                client_ref: None,
                id: record.header.id,
                revision: 1,
            })
            .collect();
        let mut state = State::default();
        state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence: 1,
                records,
                created,
                updated: Vec::new(),
                warnings: Vec::new(),
            })
            .unwrap();
        let head = Snapshot {
            sequence: 1,
            head_hash: serde_json::from_value(json!(format!("sha256:{}", "ef".repeat(32))))
                .unwrap(),
        };
        (head, state)
    }

    fn snapshot(cases: Vec<(Snapshot, State)>) -> VerifiedSnapshot {
        VerifiedSnapshot::from_states(id(Kind::Workspace, 0, 9), cases)
    }

    fn start_here(plan: &RenderPlan) -> String {
        String::from_utf8(plan.entry("Start Here.md").unwrap().content().to_vec()).unwrap()
    }

    #[test]
    fn many_cases_are_capped_with_a_count_and_lists_are_bounded() {
        let cases = (1..=MAX_CASES as u64 + 3)
            .map(|n| case(n, "internal", if n == 1 { 7 } else { 0 }))
            .collect();
        let snapshot = snapshot(cases);
        let plan = VaultRenderer
            .render(&snapshot, &ViewPolicy::default())
            .unwrap();
        let note = start_here(&plan);
        assert_eq!(
            note,
            start_here(
                &VaultRenderer
                    .render(&snapshot, &ViewPolicy::default())
                    .unwrap()
            ),
            "deterministic"
        );
        assert!(note.contains(&format!(
            "{} cases. The first {MAX_CASES} in case ID order are described here; each of the \
             other 3 has its own Overview under `Cases/<case ID>/`.",
            MAX_CASES + 3
        )));
        let described = note.matches("\n  - Case ID: ").count();
        assert_eq!(described, MAX_CASES);
        assert_eq!(note.matches(" brief --format json`").count(), MAX_CASES);
        let first = id(Kind::Case, 1, 0);
        let overview = format!("[[Cases/{first}/Overview|Case 1 )) ((x]]");
        assert!(note.contains(&format!("\n- {overview}\n")), "{note}");
        // Seven open questions: five shown, the rest linked; the answered one
        // is not pending.
        assert!(note.contains("  - Pending questions (7):\n"), "{note}");
        assert!(note.contains(&format!("    - and 2 more in {overview}\n")));
        assert!(!note.contains("Settled"));
        assert_eq!(note.matches("Question ").count(), MAX_ITEMS);
        // Empty lists say so plainly.
        for label in [
            "Outstanding reviews",
            "Unresolved blindspot reviews",
            "Retrospectives",
        ] {
            assert!(note.contains(&format!("  - {label}: none identified\n")));
        }
        assert!(note.contains(&format!(
            "    - `investigation --case {first} frontier --format json`\n"
        )));
        assert!(note.contains("- State: complete."));
        assert!(note.contains("- Drift: none identified"));
    }

    /// The table people read is the table the publisher enforces: one row
    /// per area of [`layout::VAULT_AREAS`], in order, with its guide text.
    #[test]
    fn the_ownership_section_prints_the_enforced_table() {
        let snapshot = snapshot(vec![case(1, "internal", 0)]);
        let plan = VaultRenderer
            .render(&snapshot, &ViewPolicy::default())
            .unwrap();
        let note = start_here(&plan);
        let section = note
            .split("\n## Who owns what\n")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .unwrap();
        let rows: Vec<&str> = section
            .lines()
            .filter(|line| line.starts_with("| ") && !line.starts_with("| Location"))
            .collect();
        assert_eq!(rows.len(), layout::VAULT_AREAS.len(), "{section}");
        for (row, area) in rows.iter().zip(layout::VAULT_AREAS) {
            assert!(row.ends_with(&format!(" | {} |", area.guide)), "{row}");
        }
        assert!(rows[0].starts_with("| `Start Here.md`, `Cases/` |"));
        for (row, folder) in rows[1..4].iter().zip(["Notes", "Boards", "Inbox"]) {
            assert!(row.starts_with(&format!("| `{folder}/` | You. ")), "{row}");
        }
        assert!(rows[4].starts_with("| `Attachments/` | Investigation, for previews"));
        assert!(rows[5].starts_with("| Hidden folders (names starting with `.`) | You"));
        assert!(section.contains("Your notes are not evidence"));
    }

    #[test]
    fn a_withheld_case_shows_its_id_only() {
        let snapshot = snapshot(vec![case(1, "restricted", 1), case(2, "internal", 0)]);
        let plan = VaultRenderer
            .render(&snapshot, &ViewPolicy::default())
            .unwrap();
        let note = start_here(&plan);
        let secret = id(Kind::Case, 1, 0);
        assert!(note.contains(&format!(
            "\n- [[Cases/{secret}/Overview|{secret}]]\n  - Case ID: `{secret}`\n"
        )));
        assert!(!note.contains("Case 1"), "{note}");
        assert!(note.contains("Case 2"));
        let block = note.split("\n- [[").nth(1).unwrap();
        assert!(block.contains("  - Current answer: withheld\n"), "{block}");
        assert!(!block.contains("Focus"), "{block}");
        let public = VaultRenderer
            .render(&snapshot, &ViewPolicy::for_audience(Audience::Public))
            .unwrap();
        let note = start_here(&public);
        assert!(
            !note.contains("Case 2") && !note.contains("Question 1"),
            "{note}"
        );
        assert!(note.contains("  - Pending questions: none identified\n"));
    }

    #[test]
    fn health_announces_a_partial_render_and_drift() {
        let snapshot = snapshot(vec![case(1, "internal", 0)]);
        let policy = ViewPolicy::default();
        let plan = VaultRenderer.render(&snapshot, &policy).unwrap();
        let drifted = |n: usize| {
            (0..n)
                .map(|i| {
                    let kind = if i % 2 == 0 {
                        DriftKind::Edited
                    } else {
                        DriftKind::Missing
                    };
                    (PlanPath::new(format!("Cases/c/n{i:02}.md")).unwrap(), kind)
                })
                .collect::<Vec<_>>()
        };
        let health = EntryHealth {
            partial: Some("cannot write `Cases/x`: Permission denied [[Other]]".to_owned()),
            drift: drifted(MAX_DRIFT_LISTED + 2),
        };
        let announced = VaultRenderer
            .announce(&plan, &snapshot, &policy, &health)
            .unwrap();
        assert_eq!(announced.generation(), plan.generation());
        assert_eq!(announced.entries().len(), plan.entries().len());
        for (before, after) in plan.entries().iter().zip(announced.entries()) {
            assert_eq!(before.path(), after.path());
            assert_eq!(
                before.content() == after.content(),
                before.path().as_str() != "Start Here.md"
            );
        }
        let note = start_here(&announced);
        assert!(note.contains("- State: partial. The render of this generation stopped"));
        assert!(
            note.contains("Permission denied \\[\\[Other\\]\\]"),
            "{note}"
        );
        assert!(note.contains(&format!(
            "- Drift: {} generated files changed outside the renderer (edited 6, missing 6).",
            MAX_DRIFT_LISTED + 2
        )));
        assert!(note.contains("  - `Cases/c/n00.md` (edited)\n"));
        assert!(note.contains("  - and 2 more\n"));
        assert!(note.contains("  - Resolve each with `vault reconcile`"));
        assert!(!note.contains("n11.md"));
        assert_eq!(
            crate::marker::property(note.as_bytes(), crate::marker::GENERATION_KEY).as_deref(),
            Some(plan.generation().id())
        );
        // Healthy again: the planned bytes.
        let healed = VaultRenderer
            .announce(&announced, &snapshot, &policy, &EntryHealth::default())
            .unwrap();
        assert_eq!(healed, plan);
    }
}
