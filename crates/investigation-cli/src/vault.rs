//! `vault render`, `vault status` and `vault conflicts`: publish the working
//! vault and inspect its health. `vault reconcile` resolves drift
//! ([`crate::vault_reconcile`]).
//!
//! Rendering is explicit and separate from committing. A render never changes
//! canonical state and never fails because a generated note was edited: drift
//! is left as found, reported and kept for `vault conflicts`. `status` and
//! `conflicts` read the render manifest and the snapshot heads and write
//! nothing. No command here opens a viewer.
//!
//! The vault is always rendered as one generation spanning every case.
//! `--case` narrows the paths a report lists and counts to that case's folder.

use std::{collections::BTreeMap, fmt::Write as _};

use investigation_core::{
    identity::Id,
    wire::{Failure, Snapshot},
};
use investigation_render::{
    Action, CompletedGeneration, Drift, DriftKind, Generation, ManifestStatus, RENDERER_VERSION,
    Refusal, RenderError, RenderReport, SweptAction, VaultRenderer, VaultState, VaultStatus,
    VerifiedSnapshot, foreign_files, layout, render_vault, vault_status, working_policy,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv,
    selection::{select_case, workspace_argv},
};

/// Paths listed per category in one response; counts are always complete.
const MAX_LISTED: usize = 50;
/// Drifted paths listed in text output.
const MAX_TEXT_LINES: usize = 20;

pub(crate) const AFTER_HELP: &str = "\
render publishes the vault from the current verified snapshot. Files edited,
moved or deleted outside the renderer are left as found and reported as drift;
drift never fails a render. status and conflicts read the render record only
and write nothing. reconcile resolves listed drifted paths with one explicit
disposition. Nothing here opens a viewer.
Examples:
  investigation vault render --format json
  investigation vault status --format json
  investigation vault conflicts --format json
  investigation vault reconcile PATH --disposition preserve-as-note --dry-run --format json
  investigation vault copy-map --to Boards/my-map.canvas --format json";

#[derive(clap::Subcommand)]
pub(crate) enum VaultCommand {
    /// Publish the vault from the current verified snapshot; drift is reported, not fatal.
    #[command(after_help = "Example: investigation vault render --format json")]
    Render,
    /// Compare the vault's last complete render with the current snapshot.
    #[command(after_help = "Example: investigation vault status --format json")]
    Status,
    /// List drifted generated files and where each withheld version is kept.
    #[command(
        after_help = "Example: investigation vault conflicts --format json\nNext: investigation vault reconcile PATH --disposition preserve-as-note --dry-run --format json"
    )]
    Conflicts,
    /// Resolve listed drifted files: preserve-as-note, propose-change, discard or release-path.
    #[command(after_help = crate::vault_reconcile::AFTER_HELP)]
    Reconcile(crate::vault_reconcile::ReconcileArgs),
    /// Copy a case's generated map into Boards/ as a canvas you own; never replaces a file.
    #[command(after_help = crate::vault_copy_map::AFTER_HELP)]
    CopyMap(crate::vault_copy_map::CopyMapArgs),
}

pub(crate) fn run(
    store: &mut Store,
    global: &Global,
    command: VaultCommand,
) -> Result<Output, Failure> {
    if let VaultCommand::CopyMap(args) = &command {
        return crate::vault_copy_map::run(store, global, args);
    }
    let scope = Scope::select(store, global)?;
    match command {
        VaultCommand::Render => render(store, &scope),
        VaultCommand::Status => status(store, &scope),
        VaultCommand::Conflicts => conflicts(store.workspace(), &scope),
        VaultCommand::Reconcile(args) => crate::vault_reconcile::run(store, global, args),
        VaultCommand::CopyMap(_) => unreachable!("handled before the scope is selected"),
    }
}

/// The paths a report covers: the whole vault, or one case's folder.
struct Scope {
    case: Option<Id>,
}

impl Scope {
    fn select(store: &mut Store, global: &Global) -> Result<Self, Failure> {
        let case = match global.case.as_deref() {
            Some(selector) => Some(select_case(store, Some(selector))?),
            None => None,
        };
        Ok(Self { case })
    }

    fn folder(&self) -> Option<String> {
        self.case.map(|case| format!("{}/{case}/", layout::CASES))
    }

    fn covers(&self, path: &str) -> bool {
        self.folder()
            .is_none_or(|folder| path.starts_with(folder.as_str()))
    }

    fn covers_case(&self, case: Id) -> bool {
        self.case.is_none_or(|selected| selected == case)
    }

    fn json(&self) -> Value {
        match self.case {
            Some(case) => json!({"kind":"case","case_id":case,"folder":self.folder()}),
            None => json!({"kind":"vault"}),
        }
    }
}

/// A failed render: what stopped it, that the journal is unchanged, and the
/// command to run next.
pub(crate) fn render_failure(workspace: &Workspace, error: &RenderError) -> Failure {
    let mut failure = error.to_failure();
    match error {
        RenderError::Busy { .. } => failure.with_argv(argv(workspace, &["vault", "render"])),
        RenderError::Snapshot(_) | RenderError::VaultDisabled => failure,
        _ => {
            let reason = error.to_string();
            failure.error.message = format!(
                "The render stopped: {}. Nothing in the workspace journal changed. The \
                 vault may mix generations until a render completes it; vault status shows \
                 its state.",
                reason.trim_end_matches('.')
            );
            failure.with_argv(argv(workspace, &["vault", "status"]))
        }
    }
}

pub(crate) fn argv(workspace: &Workspace, tail: &[&str]) -> Vec<String> {
    let mut argv = workspace_argv(workspace);
    argv.extend(tail.iter().map(|part| (*part).to_owned()));
    argv.extend(["--format".to_owned(), "json".to_owned()]);
    argv
}

fn next(workspace: &Workspace, tail: Option<&[&str]>) -> Value {
    json!(
        tail.map(|tail| json!({"kind":"local_read","argv":argv(workspace, tail)}))
            .into_iter()
            .collect::<Vec<_>>()
    )
}

fn next_line(text: &mut String, next: &Value) {
    if let Some(argv) = next[0]["argv"].as_array() {
        let argv: Vec<String> = argv
            .iter()
            .filter_map(|part| part.as_str().map(str::to_owned))
            .collect();
        let _ = write!(text, "\nNext: {}", display_argv(&argv));
    }
}

fn manifest_json(status: &ManifestStatus) -> Value {
    match status {
        ManifestStatus::Loaded => json!({"status":"loaded"}),
        ManifestStatus::Missing => json!({"status":"missing"}),
        ManifestStatus::Rebuilt { reason } => json!({"status":"rebuilt","reason":reason}),
    }
}

const fn refusal(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::NotProvenOwned => "not_proven_owned",
        Refusal::Drifted => "drifted",
        Refusal::NotARegularFile => "not_a_regular_file",
        Refusal::UnsafeParent => "unsafe_parent",
        Refusal::HumanOwned => "human_owned",
        Refusal::UnlistedAttachment => "unlisted_attachment",
    }
}

const fn swept(action: SweptAction) -> &'static str {
    match action {
        SweptAction::Removed => "removed",
        SweptAction::Restored => "restored",
        SweptAction::Kept => "kept",
    }
}

/// What to do about one drifted path. Nothing here acts on the vault.
const fn restore(kind: DriftKind) -> &'static str {
    match kind {
        DriftKind::Edited => {
            "vault reconcile PATH with --disposition preserve-as-note keeps the edited file in \
             Notes/Reconciled/ and regenerates the path; propose-change records a supported \
             change you state; discard (with --discard-observed) drops the edit; release-path \
             makes the file yours and moves the projection."
        }
        DriftKind::Missing => {
            "vault reconcile PATH with --disposition discard --discard-observed regenerates the \
             deleted file; release-path gives up the path and projects the record beside it."
        }
        DriftKind::Renamed => {
            "vault reconcile PATH with --disposition preserve-as-note keeps the moved file (in \
             Notes/Reconciled/ unless it is already in Notes/, Boards/ or Inbox/) and \
             regenerates the path."
        }
        DriftKind::ConflictCopy => {
            "vault reconcile PATH with --disposition preserve-as-note moves the copies into \
             Notes/Reconciled/; discard (with --discard-observed) sets them aside."
        }
    }
}

pub(crate) fn drift_json(drift: &Drift) -> Value {
    let renamed_to: Vec<Value> = drift
        .renamed_to
        .iter()
        .map(|path| {
            let first = path.split('/').next().unwrap_or_default();
            json!({"path":path,"human_owned":layout::is_human_root(first)})
        })
        .collect();
    json!({
        "path":drift.path.as_str(),
        "kind":drift.kind.as_str(),
        "projects":drift.projects,
        "expected_sha256":drift.expected,
        "observed_sha256":drift.observed,
        "generated_sha256":drift.generated,
        "retained":drift.conflict,
        "renamed_to":renamed_to,
        "conflict_copies":drift.copies,
        "since":drift.since,
        "restore":restore(drift.kind),
    })
}

fn kind_counts<'a>(drift: impl Iterator<Item = &'a Drift>) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for drift in drift {
        *counts.entry(drift.kind.as_str()).or_default() += 1;
    }
    counts
}

fn drift_lines(text: &mut String, drift: &[&Drift]) {
    for drift in drift.iter().take(MAX_TEXT_LINES) {
        let _ = write!(
            text,
            "\ndrifted  {}  {}",
            drift.path.as_str(),
            drift.kind.as_str()
        );
        if let Some(retained) = &drift.conflict {
            let _ = write!(text, "  retained {retained}");
        }
    }
    if drift.len() > MAX_TEXT_LINES {
        let _ = write!(text, "\n... and {} more", drift.len() - MAX_TEXT_LINES);
    }
}

fn capped(items: &[String]) -> &[String] {
    &items[..items.len().min(MAX_LISTED)]
}

fn render(store: &mut Store, scope: &Scope) -> Result<Output, Failure> {
    let policy = working_policy(store.workspace());
    let report = render_vault(store, &VaultRenderer, &policy, &mut |_| {})
        .map_err(|error| render_failure(store.workspace(), &error))?;
    let workspace = store.workspace();
    let status = vault_status(workspace).map_err(|error| error.to_failure())?;
    let generation = &report.publish.generation;
    Ok(render_output(
        workspace,
        scope,
        &report,
        &status,
        status.is_current(generation),
    ))
}

fn render_output(
    workspace: &Workspace,
    scope: &Scope,
    report: &RenderReport,
    status: &VaultStatus,
    complete: bool,
) -> Output {
    let generation = report.publish.generation.id();
    let outcomes: Vec<_> = report
        .publish
        .outcomes
        .iter()
        .filter(|outcome| scope.covers(outcome.path.as_str()))
        .collect();
    let count = |action: Action| {
        outcomes
            .iter()
            .filter(|outcome| outcome.action == action)
            .count()
    };
    let drift: Vec<&Drift> = report
        .drift
        .iter()
        .filter(|drift| scope.covers(drift.path.as_str()))
        .collect();
    // Refusals other than drift: foreign files at planned paths, entries in
    // human-owned areas, attachments that are not listed derivatives.
    let skipped: Vec<Value> = outcomes
        .iter()
        .filter_map(|outcome| match outcome.action {
            Action::Refused(reason) if drift.iter().all(|drift| drift.path != outcome.path) => {
                Some(json!({"path":outcome.path.as_str(),"reason":refusal(reason)}))
            }
            _ => None,
        })
        .collect();
    let unknown: Vec<String> = report
        .unknown
        .iter()
        .filter(|path| scope.covers(path))
        .cloned()
        .collect();
    let adopted = report
        .adopted
        .iter()
        .filter(|path| scope.covers(path.as_str()))
        .count();
    let swept_json: Vec<Value> = report
        .swept
        .iter()
        .take(MAX_LISTED)
        .map(|leftover| json!({"path":leftover.path,"action":swept(leftover.action)}))
        .collect();
    let counts = json!({
        "planned":outcomes.len(),
        "written":count(Action::Written),
        "unchanged":count(Action::Unchanged),
        "removed":count(Action::Removed),
        "drifted":drift.len(),
        "skipped":skipped.len(),
        "adopted":adopted,
        "unknown":unknown.len(),
        "swept":report.swept.len(),
    });
    let next = next(
        workspace,
        Some(if drift.is_empty() {
            &["vault", "status"]
        } else {
            &["vault", "conflicts"]
        }),
    );
    let value = json!({
        "generation":generation,
        "renderer":RENDERER_VERSION,
        "state":status.state.as_str(),
        "complete":complete,
        "scope":scope.json(),
        "manifest":manifest_json(&report.manifest),
        "counts":counts,
        "drift":drift.iter().map(|drift| drift_json(drift)).collect::<Vec<_>>(),
        "skipped":skipped.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "unknown":capped(&unknown),
        "swept":swept_json,
        "lists_capped_at":MAX_LISTED,
        "next":next,
    });
    let mut text = format!(
        "rendered  {generation}  {}\nwritten {}  unchanged {}  removed {}  drifted {}  skipped {}  unknown {}",
        if complete {
            "complete"
        } else {
            status.state.as_str()
        },
        counts["written"],
        counts["unchanged"],
        counts["removed"],
        counts["drifted"],
        counts["skipped"],
        counts["unknown"],
    );
    if !report.swept.is_empty() {
        let _ = write!(
            text,
            "\nswept {} leftover(s) of an interrupted render",
            report.swept.len()
        );
    }
    drift_lines(&mut text, &drift);
    next_line(&mut text, &value["next"]);
    Output::success("vault_rendered", value, text)
}

fn head_json(head: &Snapshot) -> Value {
    json!({"sequence":head.sequence,"head_hash":head.head_hash})
}

/// Why the last complete render is not the current generation, most
/// general first.
fn stale_reasons(last: Option<&CompletedGeneration>, current: &Generation) -> Vec<String> {
    let Some(last) = last else {
        return vec!["no render has completed".to_owned()];
    };
    let mut reasons = Vec::new();
    if last.renderer != current.renderer() {
        reasons.push(format!(
            "renderer changed from {} to {}",
            last.renderer,
            current.renderer()
        ));
    }
    if last.policy != current.policy() {
        reasons.push(format!(
            "view policy changed from {} to {}",
            last.policy,
            current.policy()
        ));
    }
    let rendered: BTreeMap<Id, &Snapshot> =
        last.heads.iter().map(|(id, head)| (*id, head)).collect();
    for (case, head) in current.heads() {
        match rendered.get(case) {
            None => reasons.push(format!("case {case} is not rendered yet")),
            Some(old) if *old != head => reasons.push(format!(
                "case {case} advanced from sequence {} to {}",
                old.sequence, head.sequence
            )),
            Some(_) => {}
        }
    }
    reasons
}

fn state_json(state: &VaultState) -> Value {
    match state {
        VaultState::Unrendered => json!(null),
        VaultState::Complete { generation } => json!({"generation":generation}),
        VaultState::Updating { from, to } => json!({"from":from,"to":to}),
        VaultState::Partial {
            last_complete,
            generation,
            reason,
            interrupted,
        } => json!({
            "last_complete":last_complete,
            "generation":generation,
            "reason":reason,
            "interrupted":interrupted,
        }),
    }
}

fn status(store: &mut Store, scope: &Scope) -> Result<Output, Failure> {
    let workspace = store.workspace();
    let enabled = workspace.config().vault_policy.enabled;
    let policy = working_policy(workspace);
    let status = vault_status(workspace).map_err(|error| error.to_failure())?;
    let snapshot = VerifiedSnapshot::read(store).map_err(|error| error.to_failure())?;
    let workspace = store.workspace();
    let current = Generation::new(RENDERER_VERSION, &snapshot, &policy);
    let is_current = status.is_current(&current);
    let foreign = if enabled {
        foreign_files(workspace).map_err(|error| error.to_failure())?
    } else {
        None
    };
    let freshness = match (&status.state, is_current) {
        (_, true) => "current",
        (VaultState::Unrendered, false) => "unrendered",
        (_, false) => "stale",
    };
    let reasons = if is_current {
        Vec::new()
    } else {
        stale_reasons(status.last_complete.as_ref(), &current)
    };
    let rendered: BTreeMap<Id, &Snapshot> = status
        .last_complete
        .iter()
        .flat_map(|last| last.heads.iter().map(|(id, head)| (*id, head)))
        .collect();
    let cases: Vec<Value> = current
        .heads()
        .iter()
        .filter(|(case, _)| scope.covers_case(*case))
        .map(|(case, head)| {
            let old = rendered.get(case);
            json!({
                "case_id":case,
                "rendered":old.map(|old| head_json(old)),
                "current":head_json(head),
                "up_to_date":old.is_some_and(|old| *old == head),
            })
        })
        .collect();
    let drift: Vec<&Drift> = status
        .drift
        .iter()
        .filter(|drift| scope.covers(drift.path.as_str()))
        .collect();
    // Paths handed to a person by `vault reconcile --disposition release-path`.
    let released: Vec<Value> = status
        .released
        .iter()
        .filter(|released| scope.covers(released.path.as_str()))
        .map(|released| {
            json!({
                "path":released.path.as_str(),
                "relocated_to":released.relocated_to.as_str(),
                "projects":released.projects,
                "since":released.since,
            })
        })
        .collect();
    let foreign: Option<Vec<String>> = foreign.map(|paths| {
        paths
            .into_iter()
            .filter(|path| scope.covers(path))
            .collect()
    });
    let tail: Option<&[&str]> = if !enabled {
        None
    } else if matches!(status.state, VaultState::Updating { .. }) {
        Some(&["vault", "status"])
    } else if !is_current {
        Some(&["vault", "render"])
    } else if !drift.is_empty() {
        Some(&["vault", "conflicts"])
    } else {
        None
    };
    let next = next(workspace, tail);
    let value = json!({
        "vault_enabled":enabled,
        "freshness":freshness,
        "current":is_current,
        "state":status.state.as_str(),
        "state_detail":state_json(&status.state),
        "last_complete":status.last_complete.as_ref().map(|last| json!({
            "generation":last.id,
            "renderer":last.renderer,
            "policy":last.policy,
        })),
        "current_generation":{
            "generation":current.id(),
            "renderer":current.renderer(),
            "policy":current.policy(),
        },
        "stale_reasons":reasons,
        "scope":scope.json(),
        "cases":cases,
        "drift_count":drift.len(),
        "drift_kinds":kind_counts(drift.iter().copied()),
        "released_count":released.len(),
        "released":released.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "foreign_count":foreign.as_ref().map(Vec::len),
        "foreign":foreign.as_deref().map(capped),
        "manifest":manifest_json(&status.manifest),
        "lists_capped_at":MAX_LISTED,
        "next":next,
    });
    let mut text = if enabled {
        format!(
            "vault  {freshness}  state {}  drift {}  foreign {}",
            status.state.as_str(),
            drift.len(),
            foreign.as_ref().map_or_else(
                || "unknown while a render runs".to_owned(),
                |paths| paths.len().to_string()
            ),
        )
    } else {
        "vault  disabled  this workspace has no vault (its vault policy is disabled)".to_owned()
    };
    if let VaultState::Partial { reason, .. } = &status.state {
        let _ = write!(text, "\npartial: {reason}");
    }
    for reason in reasons.iter().take(MAX_TEXT_LINES) {
        let _ = write!(text, "\nstale: {reason}");
    }
    next_line(&mut text, &value["next"]);
    Ok(Output::success("vault_status", value, text))
}

fn conflicts(workspace: &Workspace, scope: &Scope) -> Result<Output, Failure> {
    let status = vault_status(workspace).map_err(|error| error.to_failure())?;
    let drift: Vec<&Drift> = status
        .drift
        .iter()
        .filter(|drift| scope.covers(drift.path.as_str()))
        .collect();
    let first = drift.first().map(|drift| drift.path.as_str());
    let next = match first {
        None => next(workspace, Some(&["vault", "status"])),
        Some(path) => next(
            workspace,
            Some(&[
                "vault",
                "reconcile",
                path,
                "--disposition",
                "preserve-as-note",
                "--dry-run",
            ]),
        ),
    };
    let value = json!({
        "count":drift.len(),
        "kinds":kind_counts(drift.iter().copied()),
        "conflicts":drift.iter().map(|drift| drift_json(drift)).collect::<Vec<_>>(),
        "diagnosed_by":"the last render; drift since then appears after the next render",
        "conflicts_area":investigation_render::CONFLICTS_DIR,
        "guidance":"Drifted files stay exactly as found and are diagnosed on every render; \
            nothing written in the vault changes the workspace. Resolve each with vault \
            reconcile PATH --disposition preserve-as-note, propose-change, discard or \
            release-path; add --dry-run to see what it would do. No disposition infers \
            anything from the edited Markdown, and only discard, confirmed with \
            --discard-observed, removes observed bytes from the vault (they stay under \
            local/conflicts/discarded/). Render first when the vault is stale, so the \
            diagnosis is current. Files in Notes/, Boards/ and Inbox/ are yours; reconcile \
            only ever adds new files under Notes/Reconciled/.",
        "scope":scope.json(),
        "next":next,
    });
    let mut text = if drift.is_empty() {
        "conflicts  none recorded by the last render".to_owned()
    } else {
        format!(
            "conflicts  {} drifted file(s); each is left as found",
            drift.len()
        )
    };
    drift_lines(&mut text, &drift);
    next_line(&mut text, &value["next"]);
    Ok(Output::success("vault_conflicts", value, text))
}
