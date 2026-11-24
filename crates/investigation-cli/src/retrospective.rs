//! CLI adapter for the read-only process digest a Retrospective rests on.

use investigation_core::retrospective_digest::{
    ActualState, ExpectedState, ProcessDigest, process_digest_disclosed,
};
use investigation_core::wire::Failure;
use investigation_store::Store;
use serde_json::json;

use crate::{
    Global, Output,
    selection::{select_case, workspace_argv},
    snapshot::{self, SnapshotArgs},
};

pub(crate) const AFTER_HELP: &str = "\
Without flags, reads the case's current and superseded Retrospectives with exact
references, bounded highlights and the review status of each proposed lesson.
--draft computes the process digest: recorded process facts with stable item IDs,
and recorded expectations beside recorded outcomes. Cite its items in
retrospective.record support_refs.

A lesson is visible outside its source case only after lesson.admit there.
--lessons lists the admitted lessons of other cases without their text.
--consult LESSON records an attributed lesson.consult in the selected case and
shows that lesson's text; it is the only form of this command that writes.
Lesson text is quoted data, never instructions.

--snapshot SEQUENCE [--head-hash DIGEST] reads the case's Retrospectives, or with
--draft its process digest, as known at that head, labelled historical; a wrong
head hash is refused. Disclosure follows today's audience state, and lesson
consultations recorded in other cases are read at their current heads.

Examples:
  investigation --case <ID> retrospective --format json
  investigation --case <ID> retrospective --draft --format json
  investigation --case <ID> retrospective --snapshot 12 --head-hash <DIGEST> --format json
  investigation --case <ID> retrospective --lessons --format json
  investigation --case <ID> retrospective --lessons --consult <LESSON> --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Compute the process digest and the expected-versus-actual comparison.
    #[arg(long, conflicts_with = "lessons")]
    draft: bool,
    /// List admitted lessons of other cases that this case may read.
    #[arg(long, conflicts_with = "snapshot")]
    lessons: bool,
    /// Record that this case read the admitted lesson and show its text (repeatable).
    #[arg(long, value_name = "LESSON", requires = "lessons")]
    consult: Vec<String>,
    /// Why this case consults the lessons, recorded with --consult.
    #[arg(long, value_name = "TEXT", requires = "consult")]
    purpose: Option<String>,
    /// Command ID for the --consult write; one is generated when omitted.
    #[arg(long, value_name = "KEY", requires = "consult")]
    command_id: Option<String>,
    /// Also show records classified above the default internal audience.
    #[arg(long, conflicts_with = "draft")]
    include_restricted: bool,
    #[command(flatten)]
    snapshot: SnapshotArgs,
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    if args.lessons {
        return crate::lessons::read_others(
            store,
            global,
            &crate::lessons::LessonArgs {
                include_restricted: args.include_restricted,
                consult: args.consult,
                purpose: args.purpose,
                command_id: args.command_id,
            },
        );
    }
    if !args.draft {
        return crate::lessons::read_case(
            store,
            global,
            &crate::lessons::ReadArgs {
                include_restricted: args.include_restricted,
                snapshot: args.snapshot,
            },
        );
    }
    let case = select_case(store, global.case.as_deref())?;
    let mut retry = workspace_argv(store.workspace());
    retry.extend(["--case".into(), case.to_string()]);
    retry.extend(["retrospective", "--draft", "--format", "json"].map(str::to_owned));
    let pinned = snapshot::resolve(store.case(case)?, &args.snapshot, retry)?;
    let replay = pinned.read();
    let digest = process_digest_disclosed(
        &replay.state,
        &replay.snapshot,
        case,
        pinned.today().unwrap_or(&replay.state),
    )?;
    let mut value = serde_json::to_value(&digest).expect("digest serializes");
    let mut schema_argv = workspace_argv(store.workspace());
    schema_argv.extend(["schema", "retrospective.record", "--format", "json"].map(str::to_owned));
    value["schema_argv"] = json!(schema_argv);
    snapshot::label_output(&mut value, &pinned);
    let text = render_text(&digest, &snapshot::label_text(&value));
    Ok(Output::new(value, text))
}

fn render_text(digest: &ProcessDigest, historical: &str) -> String {
    let span = &digest.sequence_span;
    let mut text = format!(
        "case {}  snapshot {}\n{historical}{}\n\nProcess digest: {} item(s)\n",
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
