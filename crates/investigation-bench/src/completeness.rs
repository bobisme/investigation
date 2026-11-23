//! Grouped `next` output checked against the generator's ground truth.
//!
//! The same invariants run on the exhaustive small fixture and on the sparse
//! fixture:
//!
//! 1. The gap inventory holds exactly one cell gap for every missing
//!    (option, criterion) cell, under the hard or soft rule as the criterion
//!    is hard or soft, and nothing else of those rules.
//! 2. `next` accounts for every gap: ranked members (counting every member of
//!    a group), withheld gaps and not-ranked gaps sum to the inventory total.
//! 3. Every group `next` shows reports the missing-cell count of its
//!    criterion, and following its drill-down through every page returns
//!    exactly those cells.
//! 4. Withheld entries name exactly the hard-failed options, each with the
//!    count of its missing soft cells.
//!
//! On the exhaustive fixture the whole frontier is also paged through the CLI
//! and compared cell by cell. The sparse fixture is compared cell by cell in
//! process (199,000+ gaps), and through the CLI for the groups `next` shows.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use investigation_core::{gap_rules::enumerate_gaps, identity::Id};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

use crate::{
    fixtures::Fixture,
    measure::{Invocation, time},
};

const HARD_MISSING: &str = "decision.hard_criterion_cell_missing";
const SOFT_MISSING: &str = "decision.option_criterion_cell_missing";

fn id(value: &Value) -> Option<Id> {
    value.as_str()?.parse().ok()
}

fn run_argv(binary: &Path, root: &Path, argv: &Value) -> Result<Value, String> {
    let args: Vec<&str> = argv
        .as_array()
        .ok_or("argv is not an array")?
        .iter()
        .filter_map(Value::as_str)
        .skip(1)
        .collect();
    let (_, output) = time(binary, root, &Invocation::returned(&args))?;
    serde_json::from_slice(&output).map_err(|error| error.to_string())
}

/// Follow a frontier argv through every page and return its cell gaps.
fn cells_through_pages(binary: &Path, root: &Path, argv: &Value) -> Result<Paged, String> {
    let mut argv = argv.clone();
    if let Some(items) = argv.as_array_mut() {
        match items.iter().position(|item| item == "--limit") {
            Some(index) if index + 1 < items.len() => items[index + 1] = json!("500"),
            _ => items.extend([json!("--limit"), json!("500")]),
        }
    }
    let mut cells = BTreeSet::new();
    let mut seen = 0;
    let mut pages = 0;
    let mut current = Some(argv);
    let mut matching_total = 0;
    while let Some(argv) = current.take() {
        let page = run_argv(binary, root, &argv)?;
        pages += 1;
        matching_total = page["matching_total"].as_u64().unwrap_or(0) as usize;
        for gap in page["gaps"].as_array().into_iter().flatten() {
            seen += 1;
            if gap["rule_id"] == HARD_MISSING || gap["rule_id"] == SOFT_MISSING {
                let subjects = &gap["subject_refs"];
                if let (Some(option), Some(criterion)) =
                    (id(&subjects[0]["id"]), id(&subjects[1]["id"]))
                {
                    cells.insert((option, criterion));
                }
            }
        }
        current = page.get("next_page_argv").cloned();
    }
    if seen != matching_total {
        return Err(format!(
            "paging returned {seen} gaps, matching_total {matching_total}"
        ));
    }
    Ok(Paged { cells, seen, pages })
}

/// What paging through one frontier query returned.
struct Paged {
    cells: BTreeSet<(Id, Id)>,
    seen: usize,
    pages: usize,
}

pub fn check(binary: &Path, fixture: &Fixture, exhaustive: bool) -> Result<Value, String> {
    let truth = fixture
        .choice
        .as_ref()
        .ok_or("completeness needs a choice fixture")?;
    let mut failures = Vec::<String>::new();
    let missing = truth.missing();
    let missing_by_criterion = |criterion: Id| -> BTreeSet<(Id, Id)> {
        missing
            .iter()
            .filter(|(_, c)| *c == criterion)
            .copied()
            .collect()
    };
    let withheld_expected: BTreeMap<Id, usize> = truth
        .hard_failed
        .iter()
        .map(|option| {
            let count = missing
                .iter()
                .filter(|(o, c)| o == option && !truth.hard.contains(c))
                .count();
            (*option, count)
        })
        .filter(|(_, count)| *count > 0)
        .collect();

    // 1. The inventory, in process.
    let workspace = Workspace::open(&fixture.root).map_err(|error| error.to_string())?;
    let mut store = Store::open(workspace);
    let replay = store
        .case(fixture.case)
        .map_err(|failure| failure.error.message.clone())?;
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, fixture.case)
        .map_err(|failure| failure.error.message.clone())?;
    let mut inventory_cells = BTreeSet::new();
    let mut duplicates = 0;
    let mut wrong_rule = 0;
    for gap in &inventory.gaps {
        if gap.rule_id != HARD_MISSING && gap.rule_id != SOFT_MISSING {
            continue;
        }
        let cell = (gap.subject_refs[0].id, gap.subject_refs[1].id);
        if (gap.rule_id == HARD_MISSING) != truth.hard.contains(&cell.1) {
            wrong_rule += 1;
        }
        if !inventory_cells.insert(cell) {
            duplicates += 1;
        }
    }
    if inventory_cells != missing {
        failures.push(format!(
            "inventory cell gaps differ from ground truth: {} unexpected, {} absent",
            inventory_cells.difference(&missing).count(),
            missing.difference(&inventory_cells).count()
        ));
    }
    if duplicates > 0 || wrong_rule > 0 {
        failures.push(format!(
            "{duplicates} duplicate cell gaps, {wrong_rule} under the wrong rule"
        ));
    }

    // 2. next accounts for every gap.
    let (_, output) = time(
        binary,
        &fixture.root,
        &Invocation::new(&[
            "--case",
            &fixture.case.to_string(),
            "next",
            "--format",
            "json",
        ]),
    )?;
    let next: Value = serde_json::from_slice(&output).map_err(|error| error.to_string())?;
    let ranked = next["ranked_gap_total"].as_u64().unwrap_or(0) as usize;
    let withheld_total: usize = next["withheld"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| entry["gap_count"].as_u64().unwrap_or(0) as usize)
        .sum();
    let not_ranked_total: usize = next["not_ranked"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| entry["count"].as_u64().unwrap_or(0) as usize)
        .sum();
    let accounted = ranked + withheld_total + not_ranked_total;
    if accounted != inventory.gaps.len() {
        failures.push(format!(
            "next accounts for {accounted} gaps (ranked {ranked} + withheld {withheld_total} + not ranked {not_ranked_total}); the inventory has {}",
            inventory.gaps.len()
        ));
    }

    // 3. Each shown group against its criterion's missing cells.
    let mut groups = Vec::new();
    for recommendation in next["recommendations"].as_array().into_iter().flatten() {
        let group = &recommendation["group"];
        if group.is_null() {
            continue;
        }
        let Some(criterion) = id(&group["criterion_ref"]["id"]) else {
            failures.push("group without a criterion".into());
            continue;
        };
        let rule = group["rule_id"].as_str().unwrap_or_default();
        let expected = missing_by_criterion(criterion);
        let expected_withheld = expected
            .iter()
            .filter(|(option, _)| rule == SOFT_MISSING && truth.hard_failed.contains(option))
            .count();
        let gap_count = group["gap_count"].as_u64().unwrap_or(0) as usize;
        let withheld_count = group["withheld_count"].as_u64().unwrap_or(0) as usize;
        let ranked_count = group["ranked_count"].as_u64().unwrap_or(0) as usize;
        if gap_count != expected.len()
            || withheld_count != expected_withheld
            || ranked_count + withheld_count != gap_count
        {
            failures.push(format!(
                "group {criterion}: gap_count {gap_count} ranked {ranked_count} withheld {withheld_count}; expected {} with {expected_withheld} withheld",
                expected.len()
            ));
        }
        let Paged { cells, seen, pages } =
            cells_through_pages(binary, &fixture.root, &group["frontier_argv"])?;
        if cells != expected || seen != expected.len() {
            failures.push(format!(
                "group {criterion} drill-down returned {seen} gaps over {pages} pages, {} cells; expected {}",
                cells.len(),
                expected.len()
            ));
        }
        groups.push(json!({
            "criterion": criterion, "rule_id": rule, "gap_count": gap_count,
            "expected": expected.len(), "drill_down_pages": pages, "drill_down_gaps": seen,
        }));
    }
    if groups.is_empty() {
        failures.push("next showed no grouped recommendation".into());
    }

    // 4. Withheld entries.
    let withheld: BTreeMap<Id, usize> = next["withheld"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some((
                id(&entry["option_ref"]["id"])?,
                entry["gap_count"].as_u64()? as usize,
            ))
        })
        .collect();
    if withheld != withheld_expected {
        failures.push(format!(
            "withheld {} options / {} gaps; expected {} options / {} gaps",
            withheld.len(),
            withheld.values().sum::<usize>(),
            withheld_expected.len(),
            withheld_expected.values().sum::<usize>()
        ));
    }

    // Exhaustive: the whole frontier through the CLI, cell by cell.
    let mut cli_cells = Value::Null;
    if exhaustive {
        let Paged { cells, seen, pages } =
            cells_through_pages(binary, &fixture.root, &next["frontier_argv"])?;
        if cells != missing {
            failures.push(format!(
                "CLI frontier cells differ: {} unexpected, {} absent",
                cells.difference(&missing).count(),
                missing.difference(&cells).count()
            ));
        }
        if seen != inventory.gaps.len() {
            failures.push(format!(
                "CLI frontier paged {seen} gaps; the inventory has {}",
                inventory.gaps.len()
            ));
        }
        cli_cells = json!({"pages": pages, "gaps": seen, "cell_gaps": cells.len()});
    }

    Ok(json!({
        "fixture": fixture.name,
        "complete": failures.is_empty(),
        "failures": failures,
        "possible_cells": truth.options.len() * truth.criteria.len(),
        "recorded_cells": truth.recorded.len(),
        "expected_missing_cells": missing.len(),
        "inventory_total": inventory.gaps.len(),
        "inventory_cell_gaps": inventory_cells.len(),
        "next_accounting": {"ranked": ranked, "withheld": withheld_total, "not_ranked": not_ranked_total},
        "groups_checked": groups,
        "withheld_options": withheld.len(),
        "cli_full_frontier": cli_cells,
        "checks": if exhaustive {
            "inventory = ground truth (in process); next accounting; shown groups and drill-downs; withheld; whole CLI frontier cell by cell"
        } else {
            "inventory = ground truth (in process, every cell); next accounting; shown groups and drill-downs through the CLI; withheld"
        },
    }))
}
