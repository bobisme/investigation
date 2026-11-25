//! `brief`: a bounded handoff read from one verified case snapshot.
//!
//! The budget is the exact count of serialized UTF-8 bytes, so the result does
//! not depend on a tokenizer. The minimum envelope holds the case header, the
//! effective status, freshness, the strongest material caveat, the
//! contradiction and the next actions. It is bounded independently of case
//! size: a long list inside it is capped, and `omitted` gives the count and an
//! exact argv for the complete list. Each list section (outcomes, questions,
//! evidence, inquiries) is then filled whole item by whole item, in priority
//! order, while the output stays within budget. Whatever does not fit is
//! counted in `omitted`, and `complete_read_argv` reads everything from the
//! same snapshot. A caveat is never cut from an item that is kept.

use std::collections::BTreeMap;

use investigation_core::{
    brief_select,
    identity::{Id, Kind},
    state::{Record, State},
};
use investigation_store::journal::CaseReplay;
use serde_json::{Value, json};

use crate::views::{self, EffectiveStateInput, ViewDepth, project_record};

/// Lists inside the minimum envelope show at most this many entries.
pub(crate) const LIST_CAP: usize = 8;

/// Room left in a complete-read budget for the label a historical re-read adds
/// once the head has moved past the pinned snapshot.
const HISTORICAL_MARGIN: usize = 512;

pub(crate) const AFTER_HELP: &str = "\
A bounded handoff from one verified snapshot. --max-bytes counts serialized UTF-8
bytes of the JSON output, including the final newline. As a rough guide only, 4
bytes of English JSON are about one model token; no tokenizer is promised.
Text output lists the same items as that bounded JSON, one ID-first line per
record with indented caveats and limitations, so it is shorter than the budget;
use --format json for every field.

At standard and deep level, each observation with a quote or line selector
shows its citation: the line or lines in its source, the exact quote and its
quote_state. The citations section comes before the evidence and lists one
cite line per verified quote of current evidence in use, FILE line N \"QUOTE\"
with its source title as FILE, so a budget that cuts evidence items keeps
every quote and line. Cite each quote on its own, exactly as its cite line
prints it; never merge lines into a range or a list. Only quote_state
verified was found in the retained source bytes; any other state is shown as
recorded.

--level sets depth, not size. compact is an index of outcomes and evidence with
their status, freshness, caveat and contradiction. standard adds bounded
synopses, unresolved questions and inquiries. deep adds the full outcome records.
These names differ from show --depth (title, summary, full), which reads one
record: --level full is refused, and the repair retries with --level deep.
Each level fills the same --max-bytes, and a section keeps only whole items,
so a compact brief can be larger than a standard one: its short entries all
fit, while standard leaves out items that do not fit and counts them.
The mandatory header, strongest material caveat, contradiction and next actions
are never cut. Sections that do not fit are counted in omitted, and
complete_read_argv reads everything from the same snapshot. A budget below the
minimum returns BUDGET_TOO_SMALL with the minimum size and a repair argv.

--since TRANSACTION_ID reports what changed in meaning after that transaction:
new and withdrawn or retracted evidence, disposition changes, newly stale or
disputed records, new gaps, retrospectives and stale synopses.

--snapshot SEQUENCE [--head-hash DIGEST] reads the case as known at that head
and labels the output historical. Disclosure and byte availability follow
today's audience, withdrawal and retention state, so evidence items also show
current_evidentiary_use. The label and that field share --max-bytes, so a pinned
brief can keep fewer items than the head's brief of the same snapshot; omitted
counts each one and complete_read_argv reads them all.

Examples:
  investigation brief --case <ID> --format json
  investigation brief --case <ID> --level compact --max-bytes 8000 --format json
  investigation brief --case <ID> --since <TRANSACTION_ID> --format json
  investigation brief --case <ID> --snapshot 12 --head-hash <DIGEST> --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// How much of the case to include; compact is the shortest.
    #[arg(long, value_enum, default_value = "standard")]
    pub(crate) level: BriefLevel,
    /// Byte budget of the JSON output, including the final newline; text shows the same items.
    #[arg(long, default_value_t = 16_384)]
    pub(crate) max_bytes: usize,
    /// Report the semantic delta after this transaction instead of the case.
    #[arg(long, value_name = "TRANSACTION_ID")]
    pub(crate) since: Option<String>,
    #[command(flatten)]
    pub(crate) snapshot: crate::snapshot::SnapshotArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum BriefLevel {
    Compact,
    Standard,
    Deep,
}

impl BriefLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BriefInputs {
    pub effective: EffectiveStateInput,
    /// Per-outcome and per-evidence effective overlays, keyed by record ID.
    /// Missing entries stay unknown.
    pub record_effective: BTreeMap<Id, EffectiveStateInput>,
    /// Caller-selected IDs for current conclusions or decisions, in priority order.
    pub selected_outcomes: Vec<Id>,
    /// Exact, caller-supplied commands or links for deeper reading.
    pub next_actions: Vec<Value>,
    /// Caller-supplied, snapshot-bound omissions and known gaps.
    pub omissions: Vec<Value>,
    /// Program, workspace and case arguments that start every follow-up argv.
    pub read_prefix: Vec<String>,
    /// The outcome whose caveat and contradiction lead the brief.
    pub primary_outcome: Option<Id>,
    /// The historical-read label, when the brief is pinned before the head.
    pub historical: Option<Value>,
    /// The citation of each Observation, keyed by record ID (see `citation`).
    pub citations: BTreeMap<Id, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BriefError {
    pub code: &'static str,
    pub minimum_bytes: usize,
}

impl std::fmt::Display for BriefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: minimum serialized size is {} bytes",
            self.code, self.minimum_bytes
        )
    }
}

impl std::error::Error for BriefError {}

/// One list section of a bounded output, in priority order. Items excluded
/// by the level are counted, never listed.
pub(crate) struct Section {
    pub(crate) name: &'static str,
    pub(crate) items: Vec<Value>,
    pub(crate) level_omitted: usize,
}

impl Section {
    pub(crate) fn new(name: &'static str, items: Vec<Value>) -> Self {
        Self {
            name,
            items,
            level_omitted: 0,
        }
    }

    fn by_level(name: &'static str, count: usize) -> Self {
        Self {
            name,
            items: Vec::new(),
            level_omitted: count,
        }
    }
}

pub(crate) fn follow_up(prefix: &[String], tail: &[&str]) -> Vec<String> {
    let mut argv = prefix.to_vec();
    argv.extend(tail.iter().map(|part| (*part).to_owned()));
    argv.extend(["--format".into(), "json".into()]);
    argv
}

/// Produce a deterministic brief bounded by the exact compact JSON byte count.
/// `today` is the current head when `replay` is pinned to an earlier snapshot.
pub fn brief(
    replay: &CaseReplay,
    today: Option<&State>,
    level: BriefLevel,
    max_bytes: usize,
    inputs: &BriefInputs,
) -> Result<Value, BriefError> {
    let state = &replay.state;
    let case = brief_select::in_acceptance_order(state)
        .into_iter()
        .find(|r| r.header.id.kind() == Kind::Case);
    // An outcome whose effective freshness is not current ranks just behind
    // the leading outcome, so a budget cuts current outcomes first and the
    // brief keeps the outcome a change made stale (bn-1wre).
    let needs_review = |record: &Record| {
        inputs
            .record_effective
            .get(&record.header.id)
            .and_then(|effective| effective.freshness.as_ref())
            .and_then(|freshness| freshness["effective"].as_str())
            .is_some_and(|effective| effective != "current")
    };
    let selected: Vec<&Record> = if inputs.selected_outcomes.is_empty() {
        brief_select::outcomes_flagged_first(state, needs_review)
    } else {
        inputs
            .selected_outcomes
            .iter()
            .filter_map(|id| state.current(*id))
            .filter(|record| brief_select::is_outcome(record))
            .collect()
    };
    let effective_of = |record: &Record| {
        inputs
            .record_effective
            .get(&record.header.id)
            .cloned()
            .unwrap_or_default()
    };
    let outcomes = selected
        .iter()
        .map(|record| {
            let effective = effective_of(record);
            match level {
                BriefLevel::Compact => views::index_entry(record, replay, today, Some(&effective)),
                _ => project_record(record, replay, today, ViewDepth::Summary, &effective),
            }
        })
        .collect::<Vec<_>>();
    let questions = brief_select::unresolved_questions(state)
        .into_iter()
        .map(|record| views::index_entry(record, replay, today, None))
        .collect::<Vec<_>>();
    let full_outcomes = if level == BriefLevel::Deep {
        selected
            .iter()
            .map(|record| {
                project_record(
                    record,
                    replay,
                    today,
                    ViewDepth::Full,
                    &effective_of(record),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let evidence = brief_select::evidence(state)
        .into_iter()
        .map(|record| {
            let effective = effective_of(record);
            let mut item = match level {
                BriefLevel::Compact => views::index_entry(record, replay, today, Some(&effective)),
                _ => project_record(record, replay, today, ViewDepth::Summary, &effective),
            };
            // The compact index stays an index; standard and deep cite.
            if let Some(citation) = inputs.citations.get(&record.header.id)
                && level != BriefLevel::Compact
                && item["content_state"] != "restricted"
            {
                item["citation"] = citation.clone();
            }
            item
        })
        .collect::<Vec<_>>();
    let inquiries = brief_select::active_inquiries(state);
    let inquiries = if level == BriefLevel::Compact {
        Section::by_level("inquiries", inquiries.len())
    } else {
        Section::new(
            "inquiries",
            inquiries
                .into_iter()
                .map(|inquiry| {
                    let mut item = project_record(
                        inquiry,
                        replay,
                        today,
                        ViewDepth::Summary,
                        &EffectiveStateInput::default(),
                    );
                    if item["content_state"] != "restricted" {
                        item["desired_observations"] = inquiry.body["desired_observations"].clone();
                    }
                    item
                })
                .collect(),
        )
    };

    let prefix = &inputs.read_prefix;
    let pin = crate::snapshot::pin_argv(&replay.snapshot);
    let pinned_show = |id: Id| {
        let id = id.to_string();
        let mut tail = vec!["show", id.as_str(), "--depth", "full"];
        tail.extend(pin.iter().map(String::as_str));
        follow_up(prefix, &tail)
    };
    let case_read = case.map(|record| pinned_show(record.header.id));
    let primary_read = inputs
        .primary_outcome
        .map(pinned_show)
        .or(case_read.clone());
    let mut caps = Caps::default();
    let mut case_view = case.map(|record| {
        let mut view = project_record(record, replay, today, ViewDepth::Summary, &inputs.effective);
        if let Some(object) = view.as_object_mut() {
            // The brief states gaps and freshness once, at the top level.
            object.remove("gaps");
            object.remove("freshness");
            // Its level is stated with the limit in the inherited_trust section.
            object.remove("inherited_trust");
        }
        view
    });
    if let Some(view) = &mut case_view {
        caps.cap("case", view, case_read.clone());
    }
    let mut freshness =
        views::bounded_freshness(inputs.effective.freshness.as_ref(), ViewDepth::Summary);
    caps.cap("freshness", &mut freshness, case_read);
    let mut caveat = inputs
        .effective
        .material_caveat
        .clone()
        .unwrap_or_else(|| json!({"state":"unknown"}));
    caps.cap("material_caveat", &mut caveat, primary_read.clone());
    let mut contradiction = inputs
        .effective
        .contradiction
        .clone()
        .unwrap_or_else(|| json!({"state":"unknown"}));
    caps.cap("contradiction", &mut contradiction, primary_read);
    let mut gaps = json!(inputs.effective.gaps);
    caps.cap("gaps", &mut gaps, Some(follow_up(prefix, &["frontier"])));

    let output = json!({
        "schema_version":"inv.brief.v0-alpha",
        "ok":true,
        "result":"brief",
        "snapshot":replay.snapshot,
        "level":level.as_str(),
        "historical":inputs.historical.is_some(),
        "case":case_view,
        "effective_status":inputs.effective.status.clone().unwrap_or_else(|| json!({"state":"unknown"})),
        "freshness":freshness,
        "material_caveat":caveat,
        "contradiction":contradiction,
        "gaps":gaps,
        "omissions":inputs.omissions,
        "omitted":caps.into_entries(),
    });
    let mut output = output;
    if evidence.iter().any(|item| item.get("citation").is_some()) {
        output["citation_guide"] = json!(crate::citation::GUIDE);
    }
    if let Some(label) = &inputs.historical {
        output["historical_read"] = label.clone();
    }
    // Citation first among the evidence: every verified quote and its line,
    // after the outcomes and open questions and before the evidence items
    // that carry them, so a budget that cuts evidence keeps them.
    let citations = crate::citation::section(&evidence);
    let mut sections = vec![
        Section::new(
            "deeper_read_commands",
            inputs.next_actions.iter().take(3).cloned().collect(),
        ),
        Section::new("outcomes", outcomes),
        Section::new("unresolved_questions", questions),
        Section::new("citations", citations),
        Section::new("full_outcomes", full_outcomes),
        Section::new("evidence", evidence),
        inquiries,
    ];
    crate::inherited_trust::brief_compact(&mut sections);
    let level_name = level.as_str();
    fill(output, sections, max_bytes, |bytes| {
        let bytes = bytes
            .saturating_add(crate::inherited_trust::BRIEF_MARGIN)
            .to_string();
        let mut tail = vec![
            "brief",
            "--level",
            level_name,
            "--max-bytes",
            bytes.as_str(),
        ];
        tail.extend(pin.iter().map(String::as_str));
        follow_up(prefix, &tail)
    })
    .map(|output| crate::inherited_trust::brief_limits(output, max_bytes))
}

/// Lists capped inside the minimum envelope, reported once per section.
#[derive(Default)]
pub(crate) struct Caps {
    entries: Vec<Value>,
}

impl Caps {
    /// Cap every list inside `value`, recording the omitted count for
    /// `section` with `argv` as the complete read.
    pub(crate) fn cap(&mut self, section: &str, value: &mut Value, argv: Option<Vec<String>>) {
        let omitted = cap_lists(value);
        if omitted > 0 {
            let mut entry = json!({"section":section,"omitted":omitted,"cause":"list_cap"});
            if let Some(argv) = argv {
                entry["argv"] = json!(argv);
            }
            self.entries.push(entry);
        }
    }

    pub(crate) fn into_entries(self) -> Vec<Value> {
        self.entries
    }
}

/// Whether `key` holds a command line: one command, not a list, which runs
/// only whole.
fn is_argv(key: &str) -> bool {
    key == "argv" || key.ends_with("_argv")
}

/// Truncate every array longer than `LIST_CAP` in `value`, recording the
/// remainder beside it as `<key>_omitted`. Returns the total omitted. A
/// command line (`argv`, `*_argv`) is never cut: a truncated argv would
/// print a command that does not run.
fn cap_lists(value: &mut Value) -> usize {
    let mut omitted = 0;
    match value {
        Value::Object(object) => {
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                if is_argv(&key) {
                    continue;
                }
                if let Some(Value::Array(items)) = object.get_mut(&key)
                    && items.len() > LIST_CAP
                {
                    let extra = items.len() - LIST_CAP;
                    items.truncate(LIST_CAP);
                    omitted += extra;
                    object.insert(format!("{key}_omitted"), json!(extra));
                }
                if let Some(child) = object.get_mut(&key) {
                    omitted += cap_lists(child);
                }
            }
        }
        Value::Array(items) => {
            if items.len() > LIST_CAP {
                omitted += items.len() - LIST_CAP;
                items.truncate(LIST_CAP);
            }
            for item in items {
                omitted += cap_lists(item);
            }
        }
        _ => {}
    }
    omitted
}

/// Fill the list sections of `output` within `max_bytes`, after checking
/// that the minimum envelope fits. `complete_argv(n)` is the argv that reads
/// every item with an `n`-byte budget from the same snapshot.
pub(crate) fn fill(
    mut output: Value,
    sections: Vec<Section>,
    max_bytes: usize,
    complete_argv: impl Fn(usize) -> Vec<String>,
) -> Result<Value, BriefError> {
    let mut omitted = output["omitted"].as_array().cloned().unwrap_or_default();
    output["omitted_items"] = json!({});
    for section in &sections {
        let total = section.items.len() + section.level_omitted;
        output[section.name] = json!([]);
        output["omitted_items"][section.name] = json!(total);
        let cause = if section.level_omitted > 0 {
            "level"
        } else {
            "budget"
        };
        if total > 0 {
            omitted
                .push(json!({"section":section.name,"omitted":total,"total":total,"cause":cause}));
        }
    }
    output["omitted"] = json!(omitted);
    if sections.iter().any(|section| !section.items.is_empty()) {
        let mut complete = output.clone();
        complete["complete_read_argv"] = json!(complete_argv(usize::MAX));
        for section in &sections {
            for item in &section.items {
                append(&mut complete, section.name, item.clone());
            }
        }
        let bytes = encoded_len(&complete) + 1 + HISTORICAL_MARGIN;
        output["complete_read_argv"] = json!(complete_argv(bytes));
    }
    let minimum_bytes = encoded_len(&output);
    if max_bytes < minimum_bytes {
        return Err(BriefError {
            code: "BUDGET_TOO_SMALL",
            minimum_bytes,
        });
    }
    for section in sections {
        for item in section.items {
            let undo = append(&mut output, section.name, item);
            if encoded_len(&output) > max_bytes {
                undo.revert(&mut output);
                break;
            }
        }
    }
    let budget_cut = output["omitted"]
        .as_array()
        .is_some_and(|entries| entries.iter().any(|entry| entry["cause"] == "budget"));
    if !budget_cut && let Some(object) = output.as_object_mut() {
        object.remove("complete_read_argv");
    }
    Ok(output)
}

/// Enough to take back one appended item exactly.
pub(crate) struct Undo {
    section: String,
    position: usize,
    removed: Option<Value>,
}

impl Undo {
    fn revert(self, output: &mut Value) {
        let section = self.section.as_str();
        output[section]
            .as_array_mut()
            .expect("section is an array")
            .pop();
        let remaining = output["omitted_items"][section].as_u64().unwrap_or(0) + 1;
        output["omitted_items"][section] = json!(remaining);
        let entries = output["omitted"]
            .as_array_mut()
            .expect("omitted is an array");
        match self.removed {
            Some(entry) => entries.insert(self.position, entry),
            None => entries[self.position]["omitted"] = json!(remaining),
        }
    }
}

/// Append one item and update its section's counts; the budget is checked
/// by the caller.
fn append(output: &mut Value, section: &str, item: Value) -> Undo {
    output[section]
        .as_array_mut()
        .expect("section is an array")
        .push(item);
    let remaining = output["omitted_items"][section]
        .as_u64()
        .expect("section has an omission count")
        - 1;
    output["omitted_items"][section] = json!(remaining);
    let entries = output["omitted"]
        .as_array_mut()
        .expect("omitted is an array");
    let position = entries
        .iter()
        .position(|entry| entry["section"] == section && entry["cause"] == "budget")
        .expect("a budget section has an omission entry");
    let removed = if remaining == 0 {
        Some(entries.remove(position))
    } else {
        entries[position]["omitted"] = json!(remaining);
        None
    };
    Undo {
        section: section.to_owned(),
        position,
        removed,
    }
}

pub(crate) fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .expect("JSON values serialize")
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_truncate_nested_lists_and_count_the_rest() {
        let mut value = json!({"overview":{"affected":(0..20).collect::<Vec<_>>()},"short":[1,2]});
        assert_eq!(cap_lists(&mut value), 12);
        assert_eq!(
            value["overview"]["affected"].as_array().unwrap().len(),
            LIST_CAP
        );
        assert_eq!(value["overview"]["affected_omitted"], 12);
        assert_eq!(value["short"], json!([1, 2]));
    }

    #[test]
    fn caps_never_cut_a_command_line() {
        let argv: Vec<String> = (0..12).map(|part| part.to_string()).collect();
        let mut value = json!({"node":{"read":{"argv":argv},"complete_read_argv":argv}});
        assert_eq!(cap_lists(&mut value), 0);
        assert_eq!(value["node"]["read"]["argv"], json!(argv));
        assert_eq!(value["node"]["complete_read_argv"], json!(argv));
        assert!(value["node"]["read"].get("argv_omitted").is_none());
    }

    #[test]
    fn fill_keeps_whole_items_reports_omissions_and_reverts_exactly() {
        let sections = || {
            vec![
                Section::new("a", vec![json!("x".repeat(200)), json!("y".repeat(200))]),
                Section::by_level("b", 3),
            ]
        };
        let argv = |bytes: usize| vec![bytes.to_string()];
        let output = json!({"omitted":[]});
        let all = fill(output.clone(), sections(), usize::MAX, argv).unwrap();
        assert!(all.get("complete_read_argv").is_none());
        assert_eq!(all["a"].as_array().unwrap().len(), 2);
        assert_eq!(all["omitted"].as_array().unwrap().len(), 1);
        let error = fill(output.clone(), sections(), 1, argv).unwrap_err();
        assert_eq!(error.code, "BUDGET_TOO_SMALL");
        let floor = error.minimum_bytes;
        let minimum = fill(output.clone(), sections(), floor, argv).unwrap();
        assert_eq!(encoded_len(&minimum), floor);
        assert_eq!(minimum["omitted_items"]["a"], 2);
        let one = fill(output, sections(), floor + 250, argv).unwrap();
        assert_eq!(one["a"].as_array().unwrap().len(), 1);
        assert_eq!(one["omitted_items"]["a"], 1);
        assert_eq!(one["omitted"][0]["omitted"], 1);
        assert_eq!(one["omitted"][1]["cause"], "level");
        let needed: usize = one["complete_read_argv"][0]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(needed > encoded_len(&all));
        assert!(encoded_len(&one) <= floor + 250);
    }
}
