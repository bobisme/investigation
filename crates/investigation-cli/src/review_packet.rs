//! `review-packet`: a blinded read of a review subject from one snapshot.

use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    identity::{ResolveError, resolve_prefix},
    review_packet::{ReviewPacket, blinded_packet},
    wire::{ErrorCode, Failure},
};
use investigation_store::Store;
use serde_json::json;

use crate::{
    Global, Output, selection,
    selection::workspace_argv,
    snapshot::{self, SnapshotArgs},
};

pub(crate) const AFTER_HELP: &str = "\
A blinded packet keeps the question, the evidence and the evaluation criteria of a
Conclusion, Decision or Comparison. It omits the originating conclusion, every
confidence and likelihood, interpretive rationale, record status and author
identities, and lists each omission. Reviews of and objections to the subject are
listed by reference only. Restricted records appear by reference with no fields.
The packet writes nothing and makes no review independent: the review still
declares its context_isolation. review_basis gives the exact references a
validation review cites; packet_digest identifies what was available.

Without --case, in a workspace with several cases, the case is the one that
holds the ID. A prefix that matches records in several cases is refused with
CASE_REQUIRED, which names the cases and gives the retry with --case.

Examples:
  investigation --case <ID> review-packet <CONCLUSION_ID> --format json
  investigation --case <ID> review-packet <ID> --snapshot 12 --head-hash <DIGEST> --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// The Conclusion, Decision or Comparison to pack.
    id: String,
    #[command(flatten)]
    snapshot: SnapshotArgs,
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let case = selection::select_case_for(store, global.case.as_deref(), &args.id)?;
    let mut retry = workspace_argv(store.workspace());
    retry.extend([
        "--case".into(),
        case.to_string(),
        "review-packet".into(),
        args.id.clone(),
        "--format".into(),
        "json".into(),
    ]);
    let pinned = snapshot::resolve(store.case(case)?, &args.snapshot, retry)?;
    let replay = pinned.read();
    let id = resolve_prefix(
        &args.id,
        replay.state.records().map(|record| record.header.id),
    )
    .map_err(|error| {
        let (code, message) = match error {
            ResolveError::InvalidPrefix => (
                ErrorCode::InvalidInput,
                "Supply an ID or unique typed prefix.",
            ),
            ResolveError::NotFound => (
                ErrorCode::UnknownReference,
                "No record in this case matches that ID.",
            ),
            ResolveError::Ambiguous(_) => (
                ErrorCode::AmbiguousReference,
                "Several records match. Use a full ID.",
            ),
        };
        Failure::new(code, message).at("/id")
    })?;
    let subject = replay.state.current(id).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "No record in this case matches that ID.",
        )
        .at("/id")
    })?;
    // Disclosure follows today's classification, also for a historical read.
    let classifier = Classifier::new(pinned.today().unwrap_or(&replay.state));
    let policy = AudiencePolicy::default();
    let packet = blinded_packet(&replay.state, subject, &|record| {
        classifier.may_show(record, Field::Content, &policy)
    })?;
    let mut value = serde_json::to_value(&packet).expect("packet serializes");
    value["case_id"] = json!(case);
    value["snapshot"] = json!(replay.snapshot);
    value["historical"] = json!(pinned.label().is_some());
    if let Some(label) = pinned.label() {
        value["historical_read"] = label;
    }
    let mut schema_argv = workspace_argv(store.workspace());
    schema_argv.extend(["schema", "review.submit", "--format", "json"].map(str::to_owned));
    value["next"] = json!([{"kind":"local_read","argv":schema_argv}]);
    let text = render_text(&packet);
    Ok(Output::success("review_packet", value, text))
}

fn render_text(packet: &ReviewPacket) -> String {
    let mut text = format!(
        "Blinded packet for {} ({})  digest {}\n",
        packet.subject.reference.id, packet.subject.kind, packet.packet_digest
    );
    for (name, list) in [
        ("Question", &packet.question),
        ("Criteria", &packet.criteria),
        ("Evidence", &packet.evidence),
    ] {
        text.push_str(&format!("{name}: {} record(s)\n", list.len()));
        for item in list {
            text.push_str(&format!(
                "  {} {} ({})\n",
                item.reference.id, item.kind, item.content_state
            ));
        }
    }
    text.push_str("Omitted:\n");
    for omission in &packet.omissions {
        text.push_str(&format!("  {}: {}\n", omission.what, omission.detail));
    }
    for record in &packet.omitted_records {
        text.push_str(&format!("  {} ({})\n", record.reference.id, record.reason));
    }
    text.push_str(packet.notice);
    text
}
