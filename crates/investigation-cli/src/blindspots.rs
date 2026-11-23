//! CLI adapter for the read-only structural blindspot audit (`inv.audit.v1`).
//!
//! The audit reads one committed snapshot, runs the registered checks and
//! reports what the recorded structure shows. It writes nothing, records no
//! Review and never states that an investigation is complete.

use investigation_core::{
    blindspot::{AuditRequest, AuditResponse, audit},
    catalog::standard_catalog,
    command::RevisionRef,
    identity::{Id, ResolveError, resolve_prefix},
    wire::{Digest, ErrorCode, Failure, Snapshot},
};
use investigation_store::Store;
use serde_json::json;

use crate::{
    Global, Output, display_argv,
    selection::{select_case, workspace_argv},
};

pub(crate) const AFTER_HELP: &str = "\
A read-only, deterministic audit of the recorded case structure at one committed
snapshot. Each finding names its check, check version, stable gap_id, exact subject
revisions and the schema or method card that helps resolve it. checks_run lists
every check that ran, so a clean audit reads \"no structural gap identified under
checks X\". That is not proof of completeness: the checks cannot see unrecorded
evidence or explanations nobody has proposed. Record a semantic challenge with
review.submit (kind blindspot); see investigation guide blindspot.

--subject narrows the findings to one conclusion, comparison or decision and the
records it cites. --snapshot re-reads an earlier verified snapshot of the case, so
the same snapshot always gives the same audit and gap IDs.

Examples:
  investigation blindspots --case <ID> --format json
  investigation blindspots --case <ID> --subject <CONCLUSION_ID> --format json
  investigation blindspots --case <ID> --snapshot 12 --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Narrow to one conclusion, comparison or decision and the records it cites.
    #[arg(long, value_name = "ID")]
    subject: Option<String>,
    /// Audit the case as of this verified snapshot sequence instead of the head.
    #[arg(long, value_name = "SEQUENCE")]
    snapshot: Option<u64>,
    /// Require this head hash at --snapshot.
    #[arg(long, value_name = "DIGEST", requires = "snapshot")]
    head_hash: Option<String>,
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let case_id = select_case(store, global.case.as_deref())?;
    let mut prefix = workspace_argv(store.workspace());
    prefix.extend(["--case".into(), case_id.to_string()]);
    let head = store.case(case_id)?;
    let replay = match args.snapshot {
        None => head,
        Some(sequence) => {
            let head_hash = match &args.head_hash {
                Some(text) => text.parse::<Digest>().map_err(|error| {
                    Failure::new(ErrorCode::InvalidInput, error).at("--head-hash")
                })?,
                None => head
                    .state
                    .verified_snapshot(sequence)
                    .cloned()
                    .ok_or_else(|| {
                        let mut failure = Failure::new(
                            ErrorCode::UnknownReference,
                            "The sequence is not a verified snapshot of this case journal.",
                        )
                        .at("--snapshot");
                        failure.error.expected = Some(json!({"latest": head.snapshot}));
                        failure
                    })?,
            };
            head.at_snapshot(&Snapshot {
                sequence,
                head_hash,
            })
            .map_err(|failure| {
                let mut argv = prefix.clone();
                argv.extend(["blindspots".into(), "--format".into(), "json".into()]);
                failure.with_argv(argv)
            })?
        }
    };
    let subject = args
        .subject
        .as_deref()
        .map(|selector| resolve_subject(&replay.state, selector))
        .transpose()?;
    let mut response = audit(
        &replay.state,
        &replay.snapshot,
        case_id,
        &AuditRequest { subject },
    )?;
    decorate(&prefix, &mut response);
    let text = render(&response);
    Ok(Output::new(response, text))
}

fn resolve_subject(
    state: &investigation_core::state::State,
    selector: &str,
) -> Result<Id, Failure> {
    resolve_prefix(selector, state.records().map(|record| record.header.id)).map_err(|error| {
        match error {
            ResolveError::InvalidPrefix => Failure::new(
                ErrorCode::InvalidInput,
                "Supply a conclusion, comparison or decision ID or unique typed prefix.",
            )
            .at("--subject"),
            ResolveError::NotFound => Failure::new(
                ErrorCode::UnknownReference,
                "No record at the audited snapshot matches that ID.",
            )
            .at("--subject"),
            ResolveError::Ambiguous(candidates) => {
                let mut failure = Failure::new(
                    ErrorCode::AmbiguousReference,
                    "Several records match. Use a full ID.",
                )
                .at("--subject");
                failure.error.actual = Some(json!(candidates));
                failure
            }
        }
    })
}

fn json_argv(prefix: &[String], tail: &[&str]) -> Vec<String> {
    let mut argv = prefix.to_vec();
    argv.extend(tail.iter().map(|part| (*part).to_owned()));
    argv.extend(["--format".into(), "json".into()]);
    argv
}

fn show_argv(prefix: &[String], subject: &RevisionRef) -> Vec<String> {
    let id = subject.id.to_string();
    let revision = subject.revision.to_string();
    json_argv(prefix, &["show", &id, "--at", &revision, "--depth", "full"])
}

fn schema_argv(prefix: &[String], operation: &str) -> Option<Vec<String>> {
    standard_catalog().available_operation(operation).ok()?;
    Some(json_argv(prefix, &["schema", operation]))
}

fn decorate(prefix: &[String], response: &mut AuditResponse) {
    for finding in &mut response.checks {
        finding.inspect_argvs = finding
            .subject_refs
            .iter()
            .map(|subject| show_argv(prefix, subject))
            .collect();
        finding.schema_argv = finding
            .suggested_schema
            .as_deref()
            .and_then(|operation| schema_argv(prefix, operation));
        finding.guide_argv = finding
            .guide
            .as_deref()
            .map(|card| json_argv(prefix, &["guide", card]));
    }
    response.follow_up_argvs = vec![json_argv(prefix, &["guide", "blindspot"])];
    response
        .follow_up_argvs
        .extend(schema_argv(prefix, "review.submit"));
}

fn render(response: &AuditResponse) -> String {
    let mut text = format!(
        "case {}  snapshot {} ({})  checks {}\n",
        response.scope.case_id,
        response.snapshot.sequence,
        response.snapshot.head_hash,
        response.check_version,
    );
    if let Some(subject) = &response.scope.subject_ref {
        text.push_str(&format!(
            "scope: {}@{} and {} cited record(s)\n",
            subject.id,
            subject.revision,
            response.scope.subject_read_set.len().saturating_sub(1)
        ));
    }
    text.push_str(&response.statement);
    text.push('\n');
    for finding in &response.checks {
        let severity = serde_json::to_value(finding.severity)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        text.push_str(&format!(
            "{}\n   {} ({})  {severity}\n   {}\n",
            finding.gap_id, finding.check_id, finding.check_version, finding.why
        ));
        let subjects = finding
            .subject_refs
            .iter()
            .map(|subject| format!("{}@{}", subject.id, subject.revision))
            .collect::<Vec<_>>();
        text.push_str(&format!("   subjects: {}\n", subjects.join(", ")));
        if !finding.withheld_refs.is_empty() {
            text.push_str(&format!(
                "   withheld from this audience: {} record(s)\n",
                finding.withheld_refs.len()
            ));
        }
        if let Some(argv) = &finding.schema_argv {
            text.push_str(&format!("   Schema: {}\n", display_argv(argv)));
        }
        if let Some(argv) = &finding.guide_argv {
            text.push_str(&format!("   Guide: {}\n", display_argv(argv)));
        }
    }
    text.push_str(&response.limits);
    text
}
