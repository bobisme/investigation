//! `vault reconcile`: resolve drifted generated files with one explicit
//! disposition each ([`investigation_render::reconcile`]).
//!
//! Nothing is read from the edited Markdown to decide anything. The four
//! dispositions keep every observed byte unless `discard` is confirmed with
//! `--discard-observed`, and even then the bytes stay in the conflicts area.
//! `propose-change` is the only one that changes the workspace: it records a
//! change the caller states with `--field` and `--value`, for the few fields
//! it supports, through the same `record` path and checks as any other
//! command. Its precondition is the case revision the edited note was
//! rendered from, as the manifest records it, never the case's current
//! revision: a note rendered before the case changed is refused with
//! `REVISION_CONFLICT` before anything is recorded, so a change stated from
//! it never replaces a newer one it did not show (bn-3tpv). A note whose
//! rendered revision the manifest no longer vouches for is refused the same
//! way. A retry under the same `--command-id` returns the original receipt,
//! as `record` does: it reuses the precondition the first call recorded, and
//! once the path is already reconciled it records nothing and changes no
//! file. `--dry-run` checks the request and prints what would happen,
//! including the envelope a `propose-change` would submit, and writes
//! nothing; for `propose-change` it also runs that envelope through the
//! store's own dry run (the same lock, journal check, idempotency check,
//! validation and gates), so it refuses exactly what the real run refuses.

use std::fmt::Write as _;

use investigation_core::{
    catalog::standard_catalog,
    command::{Command as RecordCommand, Operation, RevisionRef},
    identity::{Id, Kind},
    wire::{ErrorCode, Failure},
};
use investigation_render::{
    VaultRenderer,
    reconcile::{
        Disposition, Handling, MAX_PATHS, OwnedPath, PathStep, ReconcileReport, ReconcileRequest,
        Reconciliation, owned_path, preview, reconcile_vault,
    },
    working_policy,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv, inherited_trust,
    selection::{require_selected_case, select_actor},
    shortcuts::{announce_generated_id, generated_command_id},
    vault::{argv, drift_json},
};

pub(crate) const AFTER_HELP: &str = "\
Resolve drifted generated files, one explicit disposition per request. Run
vault conflicts first: it lists each drifted path and what was observed there.
Nothing is inferred from the edited Markdown, and no observed bytes are lost:

  preserve-as-note  move the observed file(s) into Notes/Reconciled/ under a
                    new name, then regenerate the path
  propose-change    record a change you state with --field and --value (a
                    case's title, objective or scope, on its Overview note)
                    against the case revision the note was rendered from
                    (refused if the case changed since), keep the observed
                    bytes in local/conflicts/proposed/, then regenerate the path
  discard           needs --discard-observed; keep the observed bytes in
                    local/conflicts/discarded/ until you remove them, then
                    regenerate the path
  release-path      the file becomes yours where it is and is never written
                    again; the record is projected at <name>-2.md beside it
                    and generated links follow it

List paths explicitly; there is no \"all\". --dry-run prints the plan and
writes nothing. Works with the viewer closed.
Examples:
  investigation vault conflicts --format json
  investigation vault reconcile Cases/<case-id>/Questions/<id>.md --disposition preserve-as-note --dry-run --format json
  investigation vault reconcile Cases/<case-id>/Overview.md --disposition propose-change --field title --value 'Why do lookups stall?' --format json
Next: investigation vault status --format json";

/// Fields `propose-change` can submit: the case fields `case.amend` changes
/// one at a time, on the case's Overview note.
const SUPPORTED: &[(&str, &str)] = &[
    ("title", "the case title, on its Overview note"),
    ("objective", "the case objective, on its Overview note"),
    ("scope", "the case scope, on its Overview note"),
];

#[derive(clap::Args)]
pub(crate) struct ReconcileArgs {
    /// Drifted vault paths, as vault conflicts lists them; an explicit list.
    #[arg(value_name = "PATH", required = true, num_args = 1..)]
    paths: Vec<String>,
    /// What to do with them.
    #[arg(long, value_name = "DISPOSITION", value_parser = ["preserve-as-note", "propose-change", "discard", "release-path"])]
    disposition: String,
    /// Check the request and print what would happen; change nothing.
    #[arg(long)]
    dry_run: bool,
    /// Confirm discard: drop the observed edit from the vault.
    #[arg(long)]
    discard_observed: bool,
    /// propose-change: the field to change (title, objective or scope).
    #[arg(long, value_name = "FIELD")]
    field: Option<String>,
    /// propose-change: the new value, stated here; nothing is read from the note.
    #[arg(long, value_name = "TEXT")]
    value: Option<String>,
    /// propose-change: why; defaults to naming the reconciled path.
    #[arg(long, value_name = "TEXT")]
    reason: Option<String>,
    /// propose-change: command ID; generated and printed to stderr first when omitted.
    #[arg(long, value_name = "ID")]
    command_id: Option<String>,
}

fn invalid(message: impl Into<String>, at: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(at)
}

pub(crate) fn run(
    store: &mut Store,
    global: &Global,
    args: ReconcileArgs,
) -> Result<Output, Failure> {
    let disposition = Disposition::parse(&args.disposition)
        .ok_or_else(|| invalid("Unknown disposition.", "--disposition"))?;
    let proposing = disposition == Disposition::ProposeChange;
    for (given, flag) in [
        (args.field.is_some(), "--field"),
        (args.value.is_some(), "--value"),
        (args.reason.is_some(), "--reason"),
        (args.command_id.is_some(), "--command-id"),
    ] {
        if given && !proposing {
            return Err(invalid(
                format!("{flag} applies only to --disposition propose-change."),
                flag,
            ));
        }
    }
    if args.discard_observed && disposition != Disposition::Discard {
        return Err(invalid(
            "--discard-observed applies only to --disposition discard.",
            "--discard-observed",
        ));
    }
    if args.paths.len() > MAX_PATHS {
        return Err(invalid(
            format!("List at most {MAX_PATHS} paths in one request."),
            "PATH",
        ));
    }
    let request = ReconcileRequest {
        disposition,
        paths: args.paths.clone(),
        discard_observed: args.discard_observed,
    };
    // A dry run takes every step of the real run with the store in its own
    // dry-run mode, so a proposal is refused by the same checks and gates.
    if args.dry_run {
        store.set_dry_run();
    }
    // A retry of a propose-change this actor already recorded reuses the
    // recorded precondition, so the store compares the same request.
    let retry = if proposing {
        recorded_preconditions(store, global, &args)?
    } else {
        None
    };
    // The first call recorded the change and reconciled the path, and only
    // its response was lost: the store returns the original receipt, or
    // refuses a different request under the same key, and no file changes.
    if let Some((owned, recorded)) = &retry
        && !owned.drifted
    {
        let (_, envelope) = proposal(
            store,
            global,
            &args,
            &args.paths[0],
            owned.projects,
            Some(recorded.clone()),
        )?;
        let receipt = store.record(&serde_json::to_vec(&envelope).expect("envelope serializes"))?;
        let done = Reconciliation {
            disposition,
            steps: Vec::new(),
        };
        return Ok(output(
            store.workspace(),
            &args,
            &done,
            Some(proposed_json(&receipt)),
            (!args.dry_run).then_some(Ok(None)),
        ));
    }
    let recorded = retry.map(|(_, recorded)| recorded);
    let checked = preview(store.workspace(), &request).map_err(|error| error.to_failure())?;
    let envelope = if proposing {
        let [step] = checked.steps.as_slice() else {
            return Err(invalid(
                "propose-change reconciles one path at a time.",
                "PATH",
            ));
        };
        Some(proposal(
            store,
            global,
            &args,
            &step.path.to_string(),
            step.projects,
            recorded,
        )?)
    } else {
        None
    };
    if args.dry_run {
        let proposal = match &envelope {
            Some((_, envelope)) => {
                let bytes = serde_json::to_vec(envelope).expect("envelope serializes");
                let receipt = store.record(&bytes)?;
                Some(json!({"envelope":envelope,"would_result":receipt.result}))
            }
            None => None,
        };
        return Ok(output(store.workspace(), &args, &checked, proposal, None));
    }
    let mut proposed = None;
    if let Some((command_id, envelope)) = envelope {
        if args.command_id.is_none() {
            announce_generated_id(&command_id)?;
        }
        let bytes = serde_json::to_vec(&envelope).expect("envelope serializes");
        let receipt = store.record(&bytes)?;
        proposed = Some(proposed_json(&receipt));
    }
    let policy = working_policy(store.workspace());
    match reconcile_vault(store, &VaultRenderer, &policy, &request, &mut |_| {}) {
        Ok(report) => Ok(output(
            store.workspace(),
            &args,
            &report.done,
            proposed,
            Some(Ok(Some(&report))),
        )),
        // The change is recorded; say so rather than report a failure that
        // would read as "nothing committed".
        Err(error) if proposed.is_some() => Ok(output(
            store.workspace(),
            &args,
            &checked,
            proposed,
            Some(Err(error.to_failure())),
        )),
        Err(error) => Err(error.to_failure()),
    }
}

fn proposed_json(receipt: &investigation_core::wire::Receipt) -> Value {
    json!({
        "command_id":receipt.command_id,
        "transaction_id":receipt.transaction_id,
        "result":receipt.result,
        "updated":receipt.updated,
        "warnings":receipt.warnings,
    })
}

/// For a retry: the path as the manifest records it, and the preconditions
/// this actor's earlier `propose-change` under `--command-id` recorded in the
/// case the path projects. `None` when there is no such command there; a
/// command under the same key in another case is left to the store, which
/// refuses a different request.
fn recorded_preconditions(
    store: &mut Store,
    global: &Global,
    args: &ReconcileArgs,
) -> Result<Option<(OwnedPath, Vec<RevisionRef>)>, Failure> {
    let (Some(key), [path]) = (args.command_id.as_deref(), args.paths.as_slice()) else {
        return Ok(None);
    };
    let Some(owned) = owned_path(store.workspace(), path).map_err(|error| error.to_failure())?
    else {
        return Ok(None);
    };
    let Some(case) = owned.projects.filter(|id| id.kind() == Kind::Case) else {
        return Ok(None);
    };
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    let replay = store.case(case)?;
    Ok(replay
        .events
        .iter()
        .find(|verified| verified.event.command_id == key && verified.event.actor == actor)
        .map(|verified| (owned, verified.event.preconditions.clone())))
}

/// The `case.amend` envelope for the stated change at `path`, which projects
/// `projects`, with the case revision the note was rendered from as its
/// precondition ([`rendered_from`]), or the `recorded` one for a retry.
/// Refused unless the path projects a case, the field is supported and the
/// case has not changed since the render; nothing is read from the edited
/// note.
fn proposal(
    store: &mut Store,
    global: &Global,
    args: &ReconcileArgs,
    path: &str,
    projects: Option<Id>,
    recorded: Option<Vec<RevisionRef>>,
) -> Result<(String, Value), Failure> {
    let supported: Vec<Value> = SUPPORTED
        .iter()
        .map(|(field, applies)| json!({"field":field,"applies_to":applies}))
        .collect();
    let unsupported = |message: String| {
        let mut failure = invalid(message, "--field");
        failure.error.actual = Some(json!({"supported":supported}));
        failure
    };
    let (Some(field), Some(value)) = (args.field.as_deref(), args.value.as_deref()) else {
        return Err(invalid(
            "propose-change needs --field and --value: state the change; nothing is read from \
             the edited note.",
            "--field",
        ));
    };
    if !SUPPORTED.iter().any(|(name, _)| *name == field) {
        return Err(unsupported(format!(
            "propose-change cannot submit {field:?}. Supported: title, objective and scope of a \
             case, on its Overview note. Record other changes with an operation (investigation \
             schema --list), then reconcile with preserve-as-note or discard."
        )));
    }
    let Some(case) = projects.filter(|id| id.kind() == Kind::Case) else {
        return Err(unsupported(format!(
            "{} does not project a case, so propose-change has no supported field there. \
             Supported: title, objective and scope of a case, on its Overview note. Record \
             other changes with an operation (investigation schema --list), then reconcile \
             with preserve-as-note or discard.",
            path
        )));
    };
    if value.trim().is_empty() {
        return Err(invalid("--value must not be empty.", "--value"));
    }
    // The path names the case; a --case that names another one is refused.
    require_selected_case(store, global.case.as_deref(), case, path)?;
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    let preconditions = match recorded {
        Some(recorded) => recorded,
        None => vec![rendered_from(store, path, case)?],
    };
    let reason = args.reason.clone().unwrap_or_else(|| {
        format!("Proposed with vault reconcile after an edit of the generated note {path}.")
    });
    let mut payload = json!({"reason":reason});
    payload[field] = match field {
        "title" => json!(value),
        _ => json!({"state":"known","value":value}),
    };
    let catalog = standard_catalog();
    let descriptor = catalog.get("case.amend").expect("case.amend is cataloged");
    descriptor.validate_payload(&payload)?;
    let command_id = args.command_id.clone().unwrap_or_else(generated_command_id);
    let request = RecordCommand {
        schema_version: investigation_core::command::CommandVersion::V1,
        command_id: command_id.clone(),
        case_id: Some(case),
        actor,
        actor_descriptor: None,
        preconditions,
        operations: vec![Operation {
            op: "case.amend".to_owned(),
            client_ref: None,
            payload,
            extensions: None,
        }],
        session: inherited_trust::select_session(global)?,
        human_confirmation: None,
    };
    let envelope = serde_json::to_value(&request).expect("envelope serializes");
    RecordCommand::parse(&serde_json::to_vec(&envelope).expect("envelope serializes"))?;
    Ok((command_id, envelope))
}

/// The revision of `case` that the generated note at `path` was rendered
/// from, when it is still the case's current revision.
///
/// The manifest records, for each case note, the case head its last
/// generated bytes name ([`OwnedPath::basis`]); the edited note is never
/// read. The case record as of that head is what the person saw. When the
/// case record changed after it, a change stated from the note would replace
/// a newer one the note did not show, so it is refused with
/// `REVISION_CONFLICT`, and so is a note whose basis the manifest no longer
/// vouches for or the journal does not hold. The same revision is the
/// envelope's precondition, so a change committed between this check and
/// the commit is refused by the store too.
fn rendered_from(store: &mut Store, path: &str, case: Id) -> Result<RevisionRef, Failure> {
    let basis = owned_path(store.workspace(), path)
        .map_err(|error| error.to_failure())?
        .and_then(|owned| owned.basis);
    let replay = store.case(case)?;
    let current = replay
        .state
        .current(case)
        .map(|record| record.reference())
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                format!("The case {case} has no current record."),
            )
        })?;
    let rendered = basis.as_ref().and_then(|basis| {
        replay
            .events
            .iter()
            .find(|verified| verified.event.sequence == basis.sequence)
            .filter(|verified| verified.hash == basis.head_hash)
            .and_then(|_| replay.state.record_at(case, basis.sequence))
            .map(|record| record.reference())
    });
    if rendered == Some(current) {
        return Ok(current);
    }
    let show = argv(store.workspace(), &["show", &case.to_string()]);
    let then = "Nothing was recorded and no file changed. Read the case as it is now \
                (the repair command), keep the edit with --disposition preserve-as-note, \
                which renders the path from the current case, and state the change again \
                from there.";
    let message = match rendered {
        Some(rendered) => format!(
            "{path} was rendered from revision {} of case {case}, and the case is now at \
             revision {}: it changed after the render, so the edit was made against a \
             case this note no longer shows. {then}",
            rendered.revision, current.revision
        ),
        None => format!(
            "The case revision {path} was rendered from is not recorded (the note was \
             rendered under another renderer version or view policy, or before the \
             journal it names), so a change stated from it cannot be checked against \
             what it showed. {then}"
        ),
    };
    let mut failure = Failure::new(ErrorCode::RevisionConflict, message)
        .at("PATH")
        .with_argv(show);
    failure.error.expected = rendered.map(|rendered| json!(rendered));
    failure.error.actual = Some(json!(current));
    Err(failure)
}

fn step_json(step: &PathStep) -> Value {
    let observed: Vec<Value> = step
        .observed
        .iter()
        .map(|observed| {
            let (handling, to) = match &observed.handling {
                Handling::MovedToNotes { to } => ("moved_to_notes", Some(to.as_str())),
                Handling::SetAside { to } => ("set_aside", Some(to.as_str())),
                Handling::Left => ("left_in_place", None),
            };
            json!({
                "path":observed.path,
                "role":observed.role.as_str(),
                "sha256":observed.sha256,
                "handling":handling,
                "to":to,
            })
        })
        .collect();
    json!({
        "path":step.path.as_str(),
        "kind":step.kind.map(|kind| kind.as_str()),
        "projects":step.projects,
        "observed":observed,
        "regenerate":step.regenerate,
        "relocated_to":step.relocated_to.as_ref().map(|path| path.as_str()),
    })
}

fn step_line(text: &mut String, step: &PathStep, dry_run: bool) {
    let _ = write!(
        text,
        "\n{}  {}",
        step.path,
        step.kind.map_or("owned", |kind| kind.as_str())
    );
    for observed in &step.observed {
        let verb = match (&observed.handling, dry_run) {
            (Handling::MovedToNotes { to }, false) => format!("moved to {to}"),
            (Handling::MovedToNotes { to }, true) => format!("would move to {to}"),
            (Handling::SetAside { to }, false) => format!("kept at {to}"),
            (Handling::SetAside { to }, true) => format!("would keep at {to}"),
            (Handling::Left, _) => "left in place".to_owned(),
        };
        let _ = write!(
            text,
            "\n  {} {}: {verb}",
            observed.role.as_str(),
            observed.path
        );
    }
    if let Some(relocated) = &step.relocated_to {
        let _ = write!(
            text,
            "\n  released to you; record {} at {relocated}",
            if dry_run {
                "would be projected"
            } else {
                "projected"
            }
        );
    } else if step.regenerate {
        let _ = write!(
            text,
            "\n  {}",
            if dry_run {
                "would be regenerated"
            } else {
                "regenerated"
            }
        );
    }
}

fn output(
    workspace: &Workspace,
    args: &ReconcileArgs,
    reconciliation: &Reconciliation,
    proposal: Option<Value>,
    outcome: Option<Result<Option<&ReconcileReport>, Failure>>,
) -> Output {
    let dry_run = outcome.is_none();
    let remaining: Vec<Value> = match &outcome {
        Some(Ok(Some(report))) => report.remaining().into_iter().map(drift_json).collect(),
        _ => Vec::new(),
    };
    let render = match &outcome {
        Some(Ok(Some(report))) => {
            let publish = &report.render.publish;
            json!({
                "generation":publish.generation.id(),
                "written":publish.written(),
                "unchanged":publish.unchanged(),
                "removed":publish.removed(),
                "refused":publish.refused(),
                "drifted":report.render.drift.len(),
            })
        }
        _ => Value::Null,
    };
    let error = match &outcome {
        Some(Err(failure)) => json!(failure.error),
        _ => Value::Null,
    };
    let next_tail: Vec<String> = if dry_run {
        let mut tail = vec!["vault".to_owned(), "reconcile".to_owned()];
        tail.extend(args.paths.iter().cloned());
        tail.extend(["--disposition".to_owned(), args.disposition.clone()]);
        if args.discard_observed {
            tail.push("--discard-observed".to_owned());
        }
        for (flag, value) in [
            ("--field", &args.field),
            ("--value", &args.value),
            ("--reason", &args.reason),
            ("--command-id", &args.command_id),
        ] {
            if let Some(value) = value {
                tail.extend([flag.to_owned(), value.clone()]);
            }
        }
        tail
    } else if remaining.is_empty() && error.is_null() {
        vec!["vault".to_owned(), "status".to_owned()]
    } else {
        vec!["vault".to_owned(), "conflicts".to_owned()]
    };
    let tail: Vec<&str> = next_tail.iter().map(String::as_str).collect();
    let next_argv = argv(workspace, &tail);
    let value = json!({
        "disposition":reconciliation.disposition.as_str(),
        "dry_run":dry_run,
        "paths":reconciliation.steps.iter().map(step_json).collect::<Vec<_>>(),
        "proposal":proposal,
        "render":render,
        "remaining_drift":remaining,
        "clear":!dry_run && remaining.is_empty() && error.is_null(),
        "vault_error":error,
        "next":[{"kind":if dry_run { "local_write" } else { "local_read" },"argv":next_argv}],
    });
    let mut text = format!(
        "{}  {}  {} path(s)",
        if dry_run {
            "would reconcile"
        } else {
            "reconciled"
        },
        reconciliation.disposition.as_str(),
        reconciliation.steps.len()
    );
    for step in &reconciliation.steps {
        step_line(&mut text, step, dry_run);
    }
    if let Some(proposal) = value["proposal"].as_object() {
        match proposal.get("transaction_id") {
            Some(transaction) if proposal.get("result") == Some(&json!("already_committed")) => {
                let _ = write!(
                    text,
                    "\nthe proposed change was already recorded as {transaction}; this retry \
                     recorded nothing"
                );
            }
            Some(transaction) => {
                let _ = write!(text, "\nrecorded the proposed change as {transaction}");
            }
            None => text.push_str("\nwould record the proposed change in the envelope shown"),
        }
    }
    if let Some(Err(failure)) = &outcome {
        let _ = write!(
            text,
            "\nThe change is recorded, but the vault was not reconciled: {}",
            failure.error.message
        );
    }
    if !remaining.is_empty() {
        let _ = write!(text, "\nstill drifted: {}", remaining.len());
    }
    let _ = write!(text, "\nNext: {}", display_argv(&next_argv));
    Output::success(
        if dry_run {
            "vault_reconcile_preview"
        } else {
            "vault_reconciled"
        },
        value,
        text,
    )
}
