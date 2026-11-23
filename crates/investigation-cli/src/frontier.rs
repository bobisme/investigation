//! CLI adapter for the complete, filterable, snapshot-bound gap inventory.
//!
//! `frontier` lists every gap the versioned rules emit, unranked and in stable
//! `gap_id` order. It reads only; it never assigns, schedules or dispatches
//! work. External systems may translate items into their own work records.

use std::collections::{BTreeMap, BTreeSet};

use clap::builder::PossibleValuesParser;
use investigation_core::{
    advice_feedback::{AdviceState, GapAdvice},
    catalog::standard_catalog,
    command::RevisionRef,
    gap_explain::{GapExplanation, explain_gap, require_rule},
    gap_page::{
        DEFAULT_GAP_PAGE_LIMIT, GapCursor, GapFilter, GapOrdering, GapPage, GapPageRequest,
        MAX_GAP_PAGE_LIMIT,
    },
    gap_rules::{Gap, GapStatus, enumerate_gaps},
    identity::{Id, Kind},
    wire::{ErrorCode, Failure, Snapshot},
};
use investigation_store::{Store, pinned::read_gap_page};
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv,
    selection::{select_case, workspace_argv},
};

const STATUSES: [&str; 7] = [
    "ready_to_analyze",
    "requires_source",
    "requires_external_action",
    "blocked_by_quality",
    "stale_due_to_revision",
    "deferred",
    "resolved",
];

const NOT_AN_ASSIGNMENT: &str = "A frontier item is not an assignment, an instruction to run a test, proof that the suggested operation is a good method, or a claim that every alternative was found. This command never creates or dispatches work.";

pub(crate) const AFTER_HELP: &str = "\
Every gap the versioned structural rules emit at one committed snapshot, unranked
and in stable gap_id order. Deferred and blocked gaps stay listed. Use next for a
ranked top three. Items describe missing or stale recorded information; they are
not assignments and nothing is dispatched.

Filters combine with AND; repeated values of one filter combine with OR.
--kind matches a gap when any of its subjects is a record of that kind. Pass a
prefix (hyp) or a name (hypothesis). --reads matches a gap whose read set names
that record ID.

A continuation cursor binds the case, snapshot, rule version, filters and order.
Later pages read the first page's snapshot even after new writes. Resend the same
filters with --cursor, or drop --cursor to restart at the current head.

Examples:
  investigation frontier --case <ID> --format json
  investigation frontier --status requires_source,stale_due_to_revision --format json
  investigation frontier --kind hypothesis --limit 20 --format json
  investigation frontier --rule decision.option_criterion_cell_missing --reads <CRITERION_ID> --format json
  investigation frontier --explain <GAP_ID> --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Keep gaps with this status. Repeat or separate with commas.
    #[arg(
        long,
        value_name = "STATUS",
        value_delimiter = ',',
        value_parser = PossibleValuesParser::new(STATUSES)
    )]
    status: Vec<String>,
    /// Keep gaps with a subject of this record kind, such as hyp or hypothesis.
    #[arg(long, value_name = "KIND", value_delimiter = ',')]
    kind: Vec<String>,
    /// Keep gaps emitted by this rule ID.
    #[arg(long, value_name = "RULE_ID", value_delimiter = ',')]
    rule: Vec<String>,
    /// Keep gaps whose read set names this record ID.
    #[arg(long, value_name = "ID", value_delimiter = ',')]
    reads: Vec<String>,
    /// Return at most this many gaps per page (1 to 500).
    #[arg(long, value_name = "N", default_value_t = DEFAULT_GAP_PAGE_LIMIT)]
    limit: usize,
    /// Continue from a previous page's next_cursor, at that page's snapshot.
    #[arg(long, value_name = "CURSOR")]
    cursor: Option<String>,
    /// Name the rule and version that emits GAP_ID, and what it reads.
    #[arg(
        long,
        value_name = "GAP_ID",
        conflicts_with_all = ["status", "kind", "rule", "reads", "limit", "cursor"]
    )]
    explain: Option<String>,
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let case_id = select_case(store, global.case.as_deref())?;
    let base = Base {
        workspace: workspace_argv(store.workspace()),
        case: case_id.to_string(),
    };
    if let Some(gap_id) = &args.explain {
        return explain(store, &base, case_id, gap_id);
    }
    let filter = filter(&args)?;
    let request = GapPageRequest {
        filter,
        ordering: GapOrdering::GapId,
        limit: args.limit,
        cursor: args.cursor.clone(),
    };
    let page = read_gap_page(store, case_id, &request).map_err(|failure| {
        match failure.error.path.as_deref() {
            Some("/cursor") => {
                let restart = base.list(&request.filter, request.limit, None);
                failure.with_argv(restart)
            }
            Some("/limit") => {
                let limit = request.limit.clamp(1, MAX_GAP_PAGE_LIMIT);
                let retry = base.list(&request.filter, limit, request.cursor.as_deref());
                failure.with_argv(retry)
            }
            _ => failure,
        }
    })?;
    let value = page_value(&base, &page);
    let text = render_page(&value, &page);
    Ok(Output::new(value, text))
}

struct Base {
    workspace: Vec<String>,
    case: String,
}

impl Base {
    fn prefix(&self) -> Vec<String> {
        let mut argv = self.workspace.clone();
        argv.extend(["--case".into(), self.case.clone()]);
        argv
    }

    fn list(&self, filter: &GapFilter, limit: usize, cursor: Option<&str>) -> Vec<String> {
        let mut argv = self.prefix();
        argv.push("frontier".into());
        for status in &filter.statuses {
            argv.extend(["--status".into(), status_name(*status)]);
        }
        for kind in &filter.subject_kinds {
            argv.extend(["--kind".into(), kind.clone()]);
        }
        for rule in &filter.rule_ids {
            argv.extend(["--rule".into(), rule.clone()]);
        }
        for id in &filter.read_ids {
            argv.extend(["--reads".into(), id.to_string()]);
        }
        argv.extend(["--limit".into(), limit.to_string()]);
        if let Some(cursor) = cursor {
            argv.extend(["--cursor".into(), cursor.into()]);
        }
        argv.extend(["--format".into(), "json".into()]);
        argv
    }

    fn explain(&self, gap_id: &str) -> Vec<String> {
        let mut argv = self.prefix();
        argv.extend([
            "frontier".into(),
            "--explain".into(),
            gap_id.into(),
            "--format".into(),
            "json".into(),
        ]);
        argv
    }

    fn show(&self, subject: &RevisionRef) -> Vec<String> {
        let mut argv = self.workspace.clone();
        argv.extend([
            "show".into(),
            subject.id.to_string(),
            "--case".into(),
            self.case.clone(),
            "--at".into(),
            subject.revision.to_string(),
            "--depth".into(),
            "full".into(),
            "--format".into(),
            "json".into(),
        ]);
        argv
    }

    fn schema(&self, operation: &str) -> Option<Vec<String>> {
        standard_catalog().available_operation(operation).ok()?;
        let mut argv = self.workspace.clone();
        argv.extend([
            "schema".into(),
            operation.into(),
            "--format".into(),
            "json".into(),
        ]);
        Some(argv)
    }
}

fn status_name(status: GapStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("gap status serializes as a string")
}

fn filter(args: &Args) -> Result<GapFilter, Failure> {
    let statuses = args
        .status
        .iter()
        .map(|status| {
            serde_json::from_value::<GapStatus>(json!(status)).map_err(|_| {
                Failure::new(
                    ErrorCode::InvalidInput,
                    format!("Unknown status {status:?}."),
                )
                .at("--status")
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let subject_kinds = args
        .kind
        .iter()
        .map(|kind| normalize_kind(kind))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let rule_ids = args
        .rule
        .iter()
        .map(|rule| require_rule(rule, "--rule").map(|()| rule.clone()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let read_ids = args
        .reads
        .iter()
        .map(|id| {
            id.trim().parse::<Id>().map_err(|_| {
                let mut failure = Failure::new(
                    ErrorCode::InvalidInput,
                    format!("Invalid record ID {id:?}."),
                )
                .at("--reads");
                failure.error.actual = Some(json!(id));
                failure
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(GapFilter {
        statuses,
        subject_kinds,
        rule_ids,
        read_ids,
    })
}

/// A `frontier` argv whose first page reads `snapshot`, not the current head,
/// so a drill-down from another read stays on that read's snapshot.
pub(crate) fn snapshot_argv(
    workspace: &[String],
    case_id: Id,
    snapshot: &Snapshot,
    filter: &GapFilter,
) -> Vec<String> {
    let base = Base {
        workspace: workspace.to_vec(),
        case: case_id.to_string(),
    };
    let cursor = GapCursor::start(
        case_id,
        snapshot.clone(),
        filter.clone(),
        GapOrdering::GapId,
    );
    base.list(filter, DEFAULT_GAP_PAGE_LIMIT, Some(&cursor.encode()))
}

fn kind_name(kind: Kind) -> String {
    format!("{kind:?}").to_ascii_lowercase()
}

/// Accept a record kind prefix or its lowercase name, and bind the prefix.
fn normalize_kind(text: &str) -> Result<String, Failure> {
    let wanted = text.trim().to_ascii_lowercase();
    Kind::ALL
        .iter()
        .find(|kind| kind.prefix() == wanted || kind_name(**kind) == wanted)
        .map(|kind| kind.prefix().to_owned())
        .ok_or_else(|| {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!("Unknown record kind {text:?}; pass a kind prefix or name."),
            )
            .at("--kind");
            failure.error.expected = Some(json!(
                Kind::ALL
                    .iter()
                    .map(|kind| json!({"prefix": kind.prefix(), "name": kind_name(*kind)}))
                    .collect::<Vec<_>>()
            ));
            failure.error.actual = Some(json!(text));
            failure
        })
}

fn item_value(base: &Base, gap: &Gap) -> Value {
    let mut item = serde_json::to_value(gap).expect("gap serializes");
    item["inspect_argvs"] = json!(
        gap.subject_refs
            .iter()
            .map(|subject| base.show(subject))
            .collect::<Vec<_>>()
    );
    if let Some(argv) = gap
        .suggested_operation
        .as_deref()
        .and_then(|operation| base.schema(operation))
    {
        item["schema_argv"] = json!(argv);
    }
    item["explain_argv"] = json!(base.explain(&gap.gap_id));
    item
}

fn page_value(base: &Base, page: &GapPage) -> Value {
    let mut value = serde_json::to_value(page).expect("gap page serializes");
    value["gaps"] = json!(
        page.gaps
            .iter()
            .map(|gap| item_value(base, gap))
            .collect::<Vec<_>>()
    );
    let mut subjects = BTreeMap::<RevisionRef, Vec<&str>>::new();
    for gap in &page.gaps {
        for subject in &gap.subject_refs {
            subjects
                .entry(*subject)
                .or_default()
                .push(gap.gap_id.as_str());
        }
    }
    value["subjects"] = json!(
        subjects
            .into_iter()
            .map(|(subject, gap_ids)| json!({
                "id": subject.id,
                "revision": subject.revision,
                "gap_ids": gap_ids,
            }))
            .collect::<Vec<_>>()
    );
    if let Some(cursor) = &page.next_cursor {
        value["next_page_argv"] = json!(base.list(&page.filter, page.limit, Some(cursor)));
    }
    if page.newer_head.is_some() {
        value["restart_argv"] = json!(base.list(&page.filter, page.limit, None));
    }
    if let Some(limits) = value["limits"].as_array_mut() {
        limits.push(json!(NOT_AN_ASSIGNMENT));
    }
    value
}

fn argv_text(value: &Value) -> Option<String> {
    let argv = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(display_argv(&argv))
}

fn render_page(value: &Value, page: &GapPage) -> String {
    let mut text = format!(
        "case {}  snapshot {}  rules {}\n",
        page.case_id, page.snapshot.sequence, page.rule_version
    );
    let shown_end = page.position + page.gaps.len();
    text.push_str(&format!(
        "gaps {}-{} of {} matching; {} in the inventory\n",
        if page.gaps.is_empty() {
            page.position
        } else {
            page.position + 1
        },
        shown_end,
        page.matching_total,
        page.inventory_total
    ));
    for (index, gap) in page.gaps.iter().enumerate() {
        let item = &value["gaps"][index];
        text.push_str(&format!(
            "{}\n   {}  {} ({})\n   {}\n",
            gap.gap_id,
            status_name(gap.status),
            gap.rule_id,
            gap.rule_version,
            gap.reason
        ));
        let subjects = gap
            .subject_refs
            .iter()
            .map(|subject| format!("{}@{}", subject.id, subject.revision))
            .collect::<Vec<_>>();
        text.push_str(&format!("   subjects: {}\n", subjects.join(", ")));
        if let Some(advice) = &gap.advice {
            text.push_str(&advice_line(advice));
        }
        let prerequisites = gap
            .prerequisites
            .iter()
            .map(|prerequisite| {
                let state = serde_json::to_value(prerequisite.state)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                format!("{} ({state})", prerequisite.name)
            })
            .collect::<Vec<_>>();
        text.push_str(&format!(
            "   prerequisites: {}\n",
            if prerequisites.is_empty() {
                "none recorded".to_owned()
            } else {
                prerequisites.join(", ")
            }
        ));
        for route in item["inspect_argvs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(argv_text)
        {
            text.push_str(&format!("   Show: {route}\n"));
        }
        if let Some(route) = argv_text(&item["schema_argv"]) {
            text.push_str(&format!("   Schema: {route}\n"));
        }
    }
    if let Some(route) = argv_text(&value["next_page_argv"]) {
        text.push_str(&format!("Next page: {route}\n"));
    }
    if let Some(head) = &page.newer_head {
        text.push_str(&format!(
            "Writes were committed after this snapshot (head {}); drop --cursor to read them.\n",
            head.sequence
        ));
    }
    text.push_str("No detected gaps means no gap was emitted by this rule version; it does not mean the investigation is complete.\n");
    text.push_str(NOT_AN_ASSIGNMENT);
    text
}

fn explain(store: &mut Store, base: &Base, case_id: Id, gap_id: &str) -> Result<Output, Failure> {
    let list = base.list(&GapFilter::default(), DEFAULT_GAP_PAGE_LIMIT, None);
    let replay = store.case(case_id)?;
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case_id)?;
    let explanation = explain_gap(&replay.state, &inventory, gap_id)
        .map_err(|failure| failure.with_argv(list.clone()))?;
    let mut value = serde_json::to_value(&explanation).expect("gap explanation serializes");
    if let Some(gap) = &explanation.gap {
        value["gap"] = item_value(base, gap);
    }
    value["frontier_argv"] = json!(list);
    let text = render_explanation(&explanation);
    Ok(Output::new(value, text))
}

fn render_explanation(explanation: &GapExplanation) -> String {
    let rule = &explanation.rule;
    let mut text = format!(
        "{}\n   rule {} ({})  emits {}\n   condition: {}\n   reads: {}\n   persistence: {}\n",
        explanation.gap_id,
        rule.rule_id,
        rule.rule_version,
        status_name(rule.emitted_status),
        rule.condition,
        rule.inspected_fields.join("; "),
        rule.persistence,
    );
    text.push_str(&format!(
        "case {}  snapshot {}: {}\n",
        explanation.case_id,
        explanation.snapshot.sequence,
        if explanation.emitted {
            "emitted"
        } else {
            "not emitted"
        }
    ));
    for subject in &explanation.subjects {
        text.push_str(&format!(
            "   subject {}: {}\n",
            subject.id,
            subject.current_revision.map_or_else(
                || "absent from the case".to_owned(),
                |revision| format!("revision {revision}")
            )
        ));
    }
    for limit in &explanation.limits {
        text.push_str(&format!("{limit}\n"));
    }
    text.trim_end().to_owned()
}

/// One indented line naming the advice feedback behind a gap, and each reason
/// a deferral lapsed. The authored reason stays in the annotation.
pub(crate) fn advice_line(advice: &GapAdvice) -> String {
    let mut text = match advice.state {
        AdviceState::Deferred => format!(
            "   advice: deferred by {} in {}@{}; rule status {}\n",
            advice.recorded_by,
            advice.feedback_ref.id,
            advice.feedback_ref.revision,
            status_name(advice.rule_status)
        ),
        AdviceState::Resurfaced => format!(
            "   advice: deferral {}@{} by {} no longer holds\n",
            advice.feedback_ref.id, advice.feedback_ref.revision, advice.recorded_by
        ),
        AdviceState::Resumed => format!(
            "   advice: resumed by {} in {}@{}\n",
            advice.recorded_by, advice.feedback_ref.id, advice.feedback_ref.revision
        ),
    };
    for change in &advice.resurfaced_because {
        text.push_str(&format!("     {}\n", change.description));
    }
    text
}
