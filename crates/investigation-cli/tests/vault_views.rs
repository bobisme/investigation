//! The static table views of plan §12.5 (W08.07) over the Scenario A, C and
//! D golden vaults.
//!
//! - Every table in every generated note reads the same in a plain-text
//!   viewer: a header, a separator and rows with the header's cell count,
//!   no empty cell and no cell that is only a symbol.
//! - No table computes an overall score or rank.
//! - Every case has its epistemic frontier and timeline; the Overview links
//!   exactly the views planned for it; each view reads only its own case.
//! - Scenario C's comparison matrix links every recorded cell to its
//!   Comparison and shows every cell without one as unknown.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::identity::{Id, Kind};
use investigation_render::layout;

#[path = "../../investigation-render/tests/support/vault.rs"]
mod vault;

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

/// The cells of one table row, split on unescaped pipes, or `None` for a
/// line that is not a row.
fn cells(line: &str) -> Option<Vec<String>> {
    let inner = line.strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cell.push(c);
                if let Some(next) = chars.next() {
                    cell.push(next);
                }
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            _ => cell.push(c),
        }
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

/// Every table of a note as rows of cells, ignoring fenced blocks.
fn tables(note: &str) -> Vec<Vec<Vec<String>>> {
    let mut found = Vec::new();
    let mut current: Vec<Vec<String>> = Vec::new();
    let mut fenced = false;
    for line in note.lines() {
        if line
            .trim_start_matches("> ")
            .trim_start()
            .starts_with("```")
        {
            fenced = !fenced;
        }
        match (fenced, line.starts_with('|')) {
            (false, true) => current.push(
                cells(line)
                    .unwrap_or_else(|| panic!("a table line that does not end its row: {line:?}")),
            ),
            _ if !current.is_empty() => found.push(std::mem::take(&mut current)),
            _ => {}
        }
    }
    if !current.is_empty() {
        found.push(current);
    }
    found
}

/// What a plain-text viewer shows of a cell: link labels instead of link
/// syntax, code without backticks, escapes resolved.
fn plain(cell: &str) -> String {
    let mut text = cell.to_owned();
    while let Some(start) = text.find("[[") {
        let Some(end) = text[start..].find("]]").map(|end| start + end) else {
            break;
        };
        let inner = text[start + 2..end].replace("\\|", "|");
        let label = inner.rsplit('|').next().unwrap_or(&inner).to_owned();
        text.replace_range(start..end + 2, &label);
    }
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '`' => {}
            _ => out.push(c),
        }
    }
    out.trim().to_owned()
}

fn golden_notes() -> Vec<(String, String)> {
    let mut notes = Vec::new();
    for scenario in SCENARIOS {
        for (path, bytes) in fixtures::golden(scenario) {
            notes.push((
                format!("{scenario}: {path}"),
                String::from_utf8(bytes).unwrap(),
            ));
        }
    }
    notes
}

#[test]
fn every_table_reads_the_same_in_plain_text() {
    let mut view_tables = 0;
    let mut rows = 0;
    for (at, note) in golden_notes() {
        for table in tables(&note) {
            let [header, separator, body @ ..] = table.as_slice() else {
                panic!("{at}: a table without a header and separator: {table:?}");
            };
            assert!(
                separator
                    .iter()
                    .all(|cell| cell.len() >= 3 && cell.chars().all(|c| c == '-' || c == ':')),
                "{at}: separator {separator:?}"
            );
            assert_eq!(separator.len(), header.len(), "{at}: {header:?}");
            assert!(!body.is_empty(), "{at}: a table with no row: {header:?}");
            for row in body {
                assert_eq!(row.len(), header.len(), "{at}: {row:?} under {header:?}");
                for cell in row.iter().chain(header) {
                    let shown = plain(cell);
                    assert!(
                        shown.chars().any(char::is_alphanumeric),
                        "{at}: the cell {cell:?} reads as {shown:?}"
                    );
                }
            }
            if at.contains("/Views/") {
                view_tables += 1;
                rows += body.len();
            }
        }
    }
    assert!(
        view_tables >= 30 && rows >= 150,
        "{view_tables} tables, {rows} rows"
    );
}

#[test]
fn no_table_computes_an_overall_score_or_rank() {
    let words = ["score", "rank", "total", "weighted", "points"];
    for (at, note) in golden_notes() {
        for table in tables(&note) {
            for cell in &table[0] {
                let header = plain(cell).to_lowercase();
                for word in words {
                    assert!(!header.contains(word), "{at}: column {header:?}");
                }
            }
            if !at.contains("/Views/") {
                continue;
            }
            for row in &table[2..] {
                for cell in row {
                    let shown = plain(cell).to_lowercase();
                    for word in ["score", "rank", "ranked", "ranking", "weighted"] {
                        assert!(
                            !shown
                                .split(|c: char| !c.is_alphanumeric())
                                .any(|w| w == word),
                            "{at}: cell {shown:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_case_has_its_views_linked_from_the_overview_and_each_reads_only_its_case() {
    for scenario in SCENARIOS {
        let golden = fixtures::golden(scenario);
        let directory = tempfile::tempdir().unwrap();
        let workspace = fixtures::workspace(scenario, directory.path(), false);
        let (snapshot, _) = fixtures::plan(&workspace);
        for case in snapshot.cases() {
            let case_id = case.case_id();
            let folder = format!("{}/{case_id}/{}/", layout::CASES, layout::VIEWS);
            let views: BTreeSet<String> = golden
                .keys()
                .filter(|path| path.starts_with(&folder))
                .cloned()
                .collect();
            for always in ["Epistemic Frontier", "Timeline"] {
                let path = layout::case_view(case_id, always).unwrap().to_string();
                assert!(views.contains(&path), "{scenario}: no {path}");
            }
            let overview =
                std::str::from_utf8(&golden[&layout::case_overview(case_id).unwrap().to_string()])
                    .unwrap()
                    .to_owned();
            let listed: BTreeSet<String> = vault::section(&overview, "Views")
                .map(|section| {
                    vault::links(section)
                        .into_iter()
                        .map(|(target, _)| format!("{target}.md"))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(listed, views, "{scenario}: the Overview lists its views");
            let records = format!("{}/{case_id}/", layout::CASES);
            for path in &views {
                let note = std::str::from_utf8(&golden[path]).unwrap();
                assert_eq!(
                    vault::property(note, "inv_case_id").as_deref(),
                    Some(case_id.to_string().as_str()),
                    "{scenario}: {path}"
                );
                assert_eq!(vault::property(note, "inv_kind").as_deref(), Some("view"));
                for (target, _) in vault::links(note) {
                    assert!(
                        target.starts_with(&records),
                        "{scenario}: {path} links {target} outside its case"
                    );
                }
            }
        }
    }
}

/// The record ID after `/<folder>/` in a cell's link, if any.
fn linked_id(cell: &str, folder: &str) -> Option<Id> {
    let at = cell.find(&format!("/{folder}/"))? + folder.len() + 2;
    let end = cell[at..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .map_or(cell.len(), |end| at + end);
    cell[at..end].parse().ok()
}

#[test]
fn scenario_c_matrix_links_every_recorded_cell_and_shows_every_missing_cell_as_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = fixtures::workspace("scenario-c", directory.path(), false);
    let (snapshot, _) = fixtures::plan(&workspace);
    let case = snapshot.cases().next().unwrap();
    let state = case.state();
    let golden = fixtures::golden("scenario-c");
    let path = layout::case_view(case.case_id(), "Comparison Matrix")
        .unwrap()
        .to_string();
    let note = std::str::from_utf8(&golden[&path]).unwrap();

    // Current comparisons by (option, criterion).
    let mut recorded: BTreeMap<(Id, Id), Vec<Id>> = BTreeMap::new();
    for record in state.records() {
        if record.header.id.kind() != Kind::Comparison || record.body["retracted"] == true {
            continue;
        }
        let id = |path: &str| -> Id { record.body[path]["id"].as_str().unwrap().parse().unwrap() };
        recorded
            .entry((id("option_ref"), id("criterion_ref")))
            .or_default()
            .push(record.header.id);
    }

    let tables = tables(note);
    assert_eq!(tables.len(), 1, "one question with a matrix");
    let table = &tables[0];
    let criteria: Vec<Id> = table[0][1..]
        .iter()
        .map(|cell| linked_id(cell, "Criteria").unwrap())
        .collect();
    let mut linked = BTreeSet::new();
    let mut cells = 0;
    for row in &table[2..] {
        let option = linked_id(&row[0], "Options").unwrap();
        for (cell, criterion) in row[1..].iter().zip(&criteria) {
            cells += 1;
            match recorded.get(&(option, *criterion)) {
                Some(comparisons) => {
                    for comparison in comparisons {
                        assert!(
                            cell.contains(&format!("/Comparisons/{comparison}\\|")),
                            "{option} x {criterion}: {cell} does not link {comparison}"
                        );
                        linked.insert(*comparison);
                    }
                }
                None => assert!(
                    plain(cell).starts_with("unknown"),
                    "{option} x {criterion}: a cell without a comparison reads {cell:?}"
                ),
            }
        }
    }
    // Three eligible options against five active criteria, every one recorded.
    assert_eq!((table.len() - 2, criteria.len(), cells), (3, 5, 15));
    let all: BTreeSet<Id> = recorded.values().flatten().copied().collect();
    assert_eq!(
        linked, all,
        "every recorded comparison is linked from its cell"
    );
    // Hard failures and Pareto status follow the table in words.
    for words in [
        "Hard failures:",
        "No shown option has a recorded hard failure.",
        "Pareto status",
        "on the frontier only provisionally",
        "No overall score, rank or weighted total is computed.",
    ] {
        assert!(note.contains(words), "{words}");
    }
}
