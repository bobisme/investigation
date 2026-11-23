//! CLI adapter for the read-only process digest a Retrospective rests on.

use investigation_core::retrospective_digest::{
    ActualState, ExpectedState, ProcessDigest, process_digest,
};
use investigation_core::wire::Failure;
use investigation_store::Store;
use serde_json::json;

use crate::{
    Global, Output,
    selection::{select_case, workspace_argv},
};

pub(crate) const AFTER_HELP: &str = "\
The digest lists recorded process facts with stable item IDs and compares recorded
expectations with recorded outcomes. It covers recorded work only and writes nothing.
Cite its items in retrospective.record support_refs.
Example: investigation --case <ID> retrospective --draft --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Compute the process digest and the expected-versus-actual comparison.
    #[arg(long, required = true)]
    draft: bool,
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    debug_assert!(args.draft);
    let case = select_case(store, global.case.as_deref())?;
    let replay = store.case(case)?;
    let digest = process_digest(&replay.state, &replay.snapshot, case)?;
    let mut value = serde_json::to_value(&digest).expect("digest serializes");
    let mut schema_argv = workspace_argv(store.workspace());
    schema_argv.extend(["schema", "retrospective.record", "--format", "json"].map(str::to_owned));
    value["schema_argv"] = json!(schema_argv);
    let text = render_text(&digest);
    Ok(Output::new(value, text))
}

fn render_text(digest: &ProcessDigest) -> String {
    let span = &digest.sequence_span;
    let mut text = format!(
        "case {}  snapshot {}\n{}\n\nProcess digest: {} item(s)\n",
        digest.case_id,
        digest.snapshot.sequence,
        digest.coverage,
        digest.items.len()
    );
    if digest.items.is_empty() {
        text.push_str(
            "  No invalid, failed, amended, rejected, stale or superseded work is recorded.\n",
        );
    }
    for item in &digest.items {
        text.push_str(&format!(
            "  {}\n    {}  cite {}\n",
            item.id, item.fact, item.cite
        ));
    }
    text.push_str(&format!(
        "\nSequence span {}..{}  {} transaction(s)  actors {}\n",
        span.first_sequence,
        span.last_sequence,
        span.transactions,
        span.actors.join(", ")
    ));
    text.push_str("\nExpected versus actual\n");
    for entry in &digest.expected_versus_actual {
        let expected = match entry.expected_state {
            ExpectedState::Recorded => entry.expected.to_string(),
            ExpectedState::NoExpectationRecorded => "no expectation recorded".to_owned(),
            ExpectedState::Withheld => "withheld from this audience".to_owned(),
        };
        let actual = match entry.actual_state {
            ActualState::Recorded => entry.actual.to_string(),
            ActualState::NoOutcomeRecorded => "no outcome recorded".to_owned(),
        };
        text.push_str(&format!(
            "  {}\n    expected: {expected}\n    actual:   {actual}\n",
            entry.id
        ));
    }
    text.push_str(&format!("\n{}\n", digest.citation_rule));
    for note in digest.not_in_journal {
        text.push_str(&format!("Not shown: {note}\n"));
    }
    text
}
