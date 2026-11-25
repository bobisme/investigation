//! `retention preview`: what withdrawing the availability of Artifact
//! references would change, before anything changes (plan §16.5; W11.11).
//!
//! A read. For each named Artifact of the selected case it runs the
//! `artifact.expire` operation itself against a copy of the verified state
//! (`retention_ops::preview`), so a refusal is the commit's refusal and the
//! dependents are exactly the records the freshness index would then mark
//! `needs_review`. It then asks the store which obligations would still hold
//! each digest (`Store::retention_obligations`) and lists the copies of the
//! bytes it knows of. Four quantities are reported separately: the logical
//! loss on each reference, bytes still held for others, reclaimable bytes,
//! and known downstream copies. References in other cases are counted, never
//! named: deduplication grants no cross-case read.
//!
//! The simulated withdrawal is the command the caller would commit: it
//! carries the selected actor and declared session, and is checked with the
//! retained method pack bytes and the session evidence the commit reads, so a
//! pinned pack's `artifact.expire` guardrail refuses it here with the
//! commit's code and repair (bn-r6ig).
//!
//! Nothing is reclaimed by the preview or by `artifact.expire`: a withdrawn
//! reference is reported unavailable while its bytes stay stored.
//!
//! `retention reclaim` (W11.19) is the write. For Artifacts of the selected
//! case whose availability is already withdrawn it removes each stored blob
//! that nothing holds any more (`Store::reclaim_blobs`: rechecked and
//! removed under one hold of the coordination lock), keeps every record,
//! and reports what was removed, what stays held and why, and the copies it
//! cannot remove. A retry after an interruption reports removed bytes as
//! already absent.
//!
//! `retention reclaim --all` (bn-s9td) sweeps the workspace: every digest a
//! withdrawn reference in any case records, decided per digest exactly as
//! the named form decides, all under one hold of the coordination lock
//! (`Store::reclaim_withdrawn`). It is a dry run that removes nothing unless
//! `--apply` is given, and it names no record: holders and quoting records
//! belong to their cases, so the sweep only counts them.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use clap::Subcommand;
use investigation_core::{
    PROGRAM_NAME,
    guardrails::SessionEvidence,
    identity::{Id, Kind, resolve_prefix},
    retention_ops::{self, LossOutcome, PreviewWriter, ReferenceLoss},
    wire::{Digest, ErrorCode, Failure},
};
use investigation_store::{
    Store,
    store::exposure::{self, Area, Form, verdict_path},
    store::retention::{DigestObligations, ReclaimOutcome},
};
use serde_json::{Value, json};

use crate::{
    Global, Output, exposure as exposure_cli,
    inherited_trust::select_session,
    selection::{select_actor, select_case},
};

pub(crate) const AFTER_HELP: &str = "\
preview lists, before anything changes, what artifact.expire would do to each
named Artifact of the case: whether reads through it would stop or be refused,
and which records would become needs_review. It also lists the bytes other
references or live archive pins still hold, the bytes a later reclamation
could remove, and the copies of the bytes it knows of. It writes nothing.

reclaim removes the stored bytes of Artifacts whose availability was already
withdrawn, when nothing else holds them, and keeps every record. With --all it
sweeps every case and is a dry run unless --apply is given.

Examples:
  investigation retention preview art_01a20751-9140-7d72-bbd2-181cd705230d --format json
  investigation retention reclaim art_01a20751-9140-7d72-bbd2-181cd705230d --format json
  investigation retention reclaim --all --format json

Next: investigation schema artifact.expire --format json";

const PREVIEW_HELP: &str = "\
Withdrawing availability is not erasure. The Artifact record, its digest, size,
provenance and earlier revisions stay, and the stored bytes stay until a
separate reclamation finds nothing else holding them. Other references to the
same bytes, in this case or another, keep their own availability; those in
other cases are counted, never named. Quoted text, derivatives, rendered notes
and exports made earlier are copies a withdrawal leaves in place.

The withdrawal is simulated as the --actor and --session you pass, the ones
the commit should carry, so a method pack guardrail on artifact.expire refuses
here with the commit's code and repair. A human confirmation is not simulated.

Commit with a record command whose operation is artifact.expire, citing the
artifact_ref this preview lists.

Examples:
  investigation retention preview art_01a20751-9140-7d72-bbd2-181cd705230d --format json
  investigation --case case_01a20751-9140-7d72-bbd2-181cd705230d retention preview art_01a2 art_01a3 --format json

Next: investigation schema artifact.expire --format json";

const RECLAIM_HELP: &str = "\
Reclamation applies only to Artifacts of the selected case whose availability
was already withdrawn with artifact.expire; any other Artifact is refused and
nothing changes. For each digest it rechecks every holder under the workspace
coordination lock: a retained reference or adopted method pack in any case, a
journal attachment, a live archive pin, unsettled capture staging and copies in
staging not yet retired. Only a digest nothing holds is removed, and each
removal is synced before the next. Records, digests, sizes, provenance and the
journal never change. Run it again after an interruption or once a holder
ends; bytes already removed are reported as already_absent.

--all sweeps the whole workspace instead: every digest that a withdrawn
reference in any case records, each decided as above, all under one hold of
the lock. Without --apply it is a dry run: it reports would_reclaim and changes
nothing. With --apply it removes what nothing holds. It takes no --case and
names no record; holders in every case are only counted. Read-only mode
refuses it, dry run included.

Quoted text, derivatives (their own digests), the index, rendered notes,
exports, backups and filesystem free blocks are not removed; the residual list
names the ones this workspace knows of. An export is listed when it names the
digest or holds a file with exactly the released bytes, found by content.

Examples:
  investigation retention reclaim art_01a20751-9140-7d72-bbd2-181cd705230d --format json
  investigation --case case_01a20751-9140-7d72-bbd2-181cd705230d retention reclaim art_01a2 art_01a3 --format text
  investigation retention reclaim --all --format json
  investigation retention reclaim --all --apply --format json

Next: investigation check --format json";

const NOTICE: &str = "Preview only: nothing was changed. artifact.expire withdraws a reference's availability; it deletes nothing. The stored bytes stay until investigation retention reclaim finds no remaining obligation, and copies outside this workspace cannot be recalled.";

const RECLAIM_NOTICE: &str = "Only stored blobs that nothing holds were removed; every record, digest and the journal are unchanged. Copies listed as residual, and copies outside this workspace, remain.";

const SWEEP_PREVIEW_NOTICE: &str = "Dry run: nothing was changed. Run again with --apply to remove the blobs listed as would_reclaim; each is rechecked under the coordination lock first, and every record, digest and the journal stay unchanged.";

const SWEEP_LIMITATION: &str = "A workspace sweep names no record. Quoted text and derivatives of the bytes are records of their cases, and the index holds the quoted text; retention preview with --case and the Artifact IDs lists them.";

const LIMITATIONS: &[&str] = &[
    "Downstream copies are those this workspace records or holds: staged copies, blob verdicts, quoting records, the index, rendered notes and exports under exports/. Exports written elsewhere and copies sent on are not known.",
    "Removed or overwritten files can survive in free blocks, filesystem snapshots, backups and SQLite free pages and write-ahead logs.",
];

#[derive(Subcommand)]
pub(crate) enum RetentionCommand {
    /// Preview withdrawing Artifact availability: loss, held bytes, reclaimable bytes, copies.
    #[command(after_help = PREVIEW_HELP)]
    Preview(PreviewArgs),
    /// Remove stored bytes of withdrawn Artifacts that nothing else holds; keeps every record.
    #[command(after_help = RECLAIM_HELP)]
    Reclaim(ReclaimArgs),
}

#[derive(clap::Args)]
pub(crate) struct PreviewArgs {
    /// Artifact IDs of the selected case; a unique typed prefix works.
    #[arg(value_name = "ARTIFACT", required = true)]
    artifacts: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct ReclaimArgs {
    /// Artifact IDs of the selected case; a unique typed prefix works.
    #[arg(
        value_name = "ARTIFACT",
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    artifacts: Vec<String>,
    /// Sweep every case for withdrawn bytes nothing holds; a dry run unless --apply is given.
    #[arg(long)]
    all: bool,
    /// With --all, remove the blobs the sweep finds that nothing holds.
    #[arg(long, requires = "all", conflicts_with = "artifacts")]
    apply: bool,
}

pub(crate) fn run(
    store: &mut Store,
    global: &Global,
    command: RetentionCommand,
) -> Result<Output, Failure> {
    match command {
        RetentionCommand::Preview(args) => preview(store, global, &args),
        RetentionCommand::Reclaim(args) if args.all => sweep(store, global, args.apply),
        RetentionCommand::Reclaim(args) => reclaim(store, global, &args.artifacts),
    }
}

/// The Artifacts `selectors` name in `case`, in order, without repeats.
fn resolve_artifacts(
    state: &investigation_core::state::State,
    case: Id,
    selectors: &[String],
) -> Result<Vec<Id>, Failure> {
    let mut ids = Vec::new();
    for (index, selector) in selectors.iter().enumerate() {
        let id = resolve_prefix(
            selector,
            state
                .records_of_kind(Kind::Artifact)
                .filter(|record| record.header.case_id == case)
                .map(|record| record.header.id),
        )
        .map_err(|error| {
            use investigation_core::identity::ResolveError;
            let code = match error {
                ResolveError::InvalidPrefix => ErrorCode::InvalidInput,
                ResolveError::NotFound => ErrorCode::UnknownReference,
                ResolveError::Ambiguous(_) => ErrorCode::AmbiguousReference,
            };
            Failure::new(code, "Supply a unique Artifact ID from this case.")
                .at(format!("/artifacts/{index}"))
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn preview(store: &mut Store, global: &Global, args: &PreviewArgs) -> Result<Output, Failure> {
    let case = select_case(store, global.case.as_deref())?;
    // The writer the commit would have: the selected actor (or the preview's
    // own when none is configured; the commit would then need one) and the
    // declared session, with the reads recorded for them.
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)
        .unwrap_or_else(|_| PreviewWriter::unguarded().actor);
    let session = select_session(global)?;
    let current = session
        .as_ref()
        .map(|session| store.session_reads(&actor, session))
        .transpose()?;
    let writer_view = json!({"actor": actor, "session": session, "human_confirmation": null});
    // One verified load gives the case and the obligations under one hold of
    // the coordination lock (the holder scan, timed by the bench).
    let mut losses = Vec::new();
    let (replay, obligations) = store.case_with_obligations(case, |replay, packs| {
        let ids = resolve_artifacts(&replay.state, case, &args.artifacts)?;
        let sessions = SessionEvidence {
            current,
            ledger: replay.session_ledger(),
        };
        let writer = PreviewWriter {
            actor,
            session,
            packs: Some(packs),
            sessions: Some(&sessions),
        };
        losses = retention_ops::preview_as(&replay.state, case, &ids, &writer);
        let releasing = losses
            .iter()
            .filter(|loss| loss.outcome == LossOutcome::BecomesUnavailable);
        let released: BTreeSet<Id> = releasing.clone().map(|loss| loss.artifact_ref.id).collect();
        let digests: BTreeSet<Digest> = releasing.filter_map(|loss| loss.digest.clone()).collect();
        Ok((digests, released))
    })?;
    let releasing: Vec<&ReferenceLoss> = losses
        .iter()
        .filter(|loss| loss.outcome == LossOutcome::BecomesUnavailable)
        .collect();
    let root = store.workspace().root().to_owned();

    let references: Vec<Value> = losses.iter().map(reference_view).collect();
    let lost_bytes: u64 = releasing.iter().filter_map(|loss| loss.size_bytes).sum();
    let mut held = Vec::new();
    let mut waiting = Vec::new();
    let mut reclaimable = Vec::new();
    let mut not_stored = Vec::new();
    for entry in &obligations {
        if !entry.stored {
            not_stored.push(json!({"digest": entry.digest, "size_bytes": entry.size_bytes}));
        } else if entry.held_by_records() {
            held.push(held_view(case, entry));
        } else if entry.reclaimable() {
            reclaimable.push(json!({"digest": entry.digest, "size_bytes": entry.size_bytes}));
        } else {
            waiting.push(waiting_view(entry));
        }
    }
    let sum = |items: &[Value]| -> u64 {
        items
            .iter()
            .filter_map(|item| item["size_bytes"].as_u64())
            .sum()
    };
    let copies = downstream(store, &replay.state, &root, &releasing);
    let outside = exposure_cli::outside(store);
    let snapshot = replay.snapshot.clone();
    investigation_store::store::free_later(replay);

    let next = json!([{
        "argv": [PROGRAM_NAME, "schema", retention_ops::OPERATION, "--format", "json"],
        "kind": "local_read",
        "effect": "Prints the artifact.expire payload and a valid example; commit it with investigation record.",
    }]);

    let mut lines = vec![format!(
        "retention preview  case {case}  nothing was changed"
    )];
    for loss in &losses {
        let mut line = format!(
            "{:<20} {}  {}",
            outcome_word(loss.outcome),
            loss.artifact_ref.id,
            loss.digest.as_ref().map_or("no digest", Digest::as_str)
        );
        if let Some(refusal) = &loss.refusal {
            line.push_str(&format!("  ({})", refusal.message));
        }
        let blockers = loss
            .refusal
            .as_ref()
            .and_then(|refusal| refusal.expected.as_ref())
            .and_then(|expected| expected["guardrail_blockers"].as_array());
        for blocker in blockers.into_iter().flatten() {
            line.push_str(&format!(
                "\n  guardrail {}/{} requires {}; repair: {}",
                blocker["pack_id"].as_str().unwrap_or("?"),
                blocker["guardrail"].as_str().unwrap_or("?"),
                blocker["requirement"].as_str().unwrap_or("?"),
                blocker["repair"]
            ));
        }
        for warning in &loss.warnings {
            line.push_str(&format!("\n  warning: {warning}"));
        }
        if !loss.dependents.is_empty() {
            line.push_str(&format!(
                "  {} record(s) would need review",
                loss.dependents.len()
            ));
        }
        lines.push(line);
    }
    lines.push(format!(
        "logical loss       {} reference(s), {} byte(s) no longer readable through them",
        releasing.len(),
        lost_bytes
    ));
    lines.push(format!(
        "held for others    {} digest(s), {} byte(s) stay stored for other references",
        held.len(),
        sum(&held)
    ));
    if !waiting.is_empty() {
        lines.push(format!(
            "waiting            {} digest(s), {} byte(s) held by a live archive pin or capture staging",
            waiting.len(),
            sum(&waiting)
        ));
    }
    lines.push(format!(
        "reclaimable        {} digest(s), {} byte(s); nothing is reclaimed by this preview or by artifact.expire",
        reclaimable.len(),
        sum(&reclaimable)
    ));
    copy_lines(&mut lines, "copy", &copies);
    lines.push(NOTICE.to_owned());
    lines.push(format!(
        "Next: {PROGRAM_NAME} schema artifact.expire --format json"
    ));

    Ok(Output::success(
        "retention_previewed",
        json!({
            "writes": "nothing",
            "case_id": case,
            "snapshot": snapshot,
            "simulated_writer": writer_view,
            "references": references,
            "logical_loss": {
                "references": releasing.len(),
                "bytes": lost_bytes,
            },
            "held_for_others": {"digests": held, "bytes": sum(&held)},
            "waiting": {"digests": waiting, "bytes": sum(&waiting)},
            "reclaimable": {
                "digests": reclaimable,
                "bytes": sum(&reclaimable),
                "reclamation": "not_performed",
            },
            "not_stored": not_stored,
            "downstream_copies": copies,
            "outside": outside,
            "limitations": LIMITATIONS,
            "notice": NOTICE,
            "next": next,
        }),
        lines.join("\n"),
    ))
}

fn copy_lines(lines: &mut Vec<String>, word: &str, copies: &[Value]) {
    for copy in copies {
        lines.push(format!(
            "{word}  {:<15} {}",
            copy["kind"].as_str().unwrap_or_default(),
            copy["path"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| copy["record"]["id"].as_str().map(str::to_owned))
                .unwrap_or_default()
        ));
    }
}

/// One digest a live pin or capture staging holds for now.
fn waiting_view(entry: &DigestObligations) -> Value {
    json!({
        "digest": entry.digest,
        "size_bytes": entry.size_bytes,
        "archive_pins": entry.archive_pins,
        "unsettled_staging": entry.unsettled_staging,
        "staged_copies": entry.staged_copies,
    })
}

const fn reclaim_word(outcome: ReclaimOutcome) -> &'static str {
    match outcome {
        ReclaimOutcome::Reclaimed => "reclaimed",
        ReclaimOutcome::AlreadyAbsent => "already_absent",
        ReclaimOutcome::HeldByRecords => "held_for_others",
        ReclaimOutcome::Waiting => "waiting",
        ReclaimOutcome::NotWithdrawn => "not_withdrawn",
        ReclaimOutcome::WouldReclaim => "would_reclaim",
    }
}

/// Reclamations grouped by outcome word, each group with its byte total.
/// `held` renders one digest that records still hold.
fn grouped(
    done: &[investigation_store::store::retention::Reclamation],
    held: impl Fn(&DigestObligations) -> Value,
) -> BTreeMap<&'static str, Value> {
    let mut groups: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for reclamation in done {
        let entry = &reclamation.obligations;
        let view = match reclamation.outcome {
            ReclaimOutcome::HeldByRecords => held(entry),
            ReclaimOutcome::Waiting => waiting_view(entry),
            _ => json!({"digest": entry.digest, "size_bytes": entry.size_bytes}),
        };
        groups
            .entry(reclaim_word(reclamation.outcome))
            .or_default()
            .push(view);
    }
    [
        "reclaimed",
        "would_reclaim",
        "already_absent",
        "held_for_others",
        "waiting",
    ]
    .into_iter()
    .map(|name| {
        let items = groups.remove(name).unwrap_or_default();
        let bytes: u64 = items
            .iter()
            .filter_map(|item| item["size_bytes"].as_u64())
            .sum();
        (name, json!({"digests": items, "bytes": bytes}))
    })
    .collect()
}

fn argv_line(next: &Value) -> String {
    next[0]["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The step after a reclamation: `check` once nothing waits, otherwise
/// retiring staging (when staged copies hold bytes) and the retry `argv`.
fn after_reclaim(
    done: &[investigation_store::store::retention::Reclamation],
    retry: Vec<String>,
) -> Value {
    let waiting = done
        .iter()
        .any(|reclamation| reclamation.outcome == ReclaimOutcome::Waiting);
    if !waiting {
        return json!([{
            "argv": [PROGRAM_NAME, "check", "--format", "json"],
            "kind": "local_read",
            "effect": "Verifies every case and every blob a record still holds; removed bytes only withdrawn references record are counted as reclaimed_blobs.",
        }]);
    }
    let retry = json!({
        "argv": retry,
        "kind": "local_write",
        "effect": "Retries the reclamation once the archive pin or capture staging that holds the bytes has ended.",
    });
    let staged = done
        .iter()
        .any(|reclamation| !reclamation.obligations.staged_copies.is_empty());
    if staged {
        json!([{
            "argv": [PROGRAM_NAME, "repair", "staging", "--format", "json"],
            "kind": "local_write",
            "effect": "Retires finalized capture staging, whose copies of the bytes hold them until then.",
        }, retry])
    } else {
        json!([retry])
    }
}

/// `retention reclaim --all`: every withdrawn digest of the workspace,
/// decided under one hold of the coordination lock; a dry run unless
/// `apply`. Names no record: every holder is only counted.
fn sweep(store: &mut Store, global: &Global, apply: bool) -> Result<Output, Failure> {
    if global.case.is_some() {
        let mut argv = vec![PROGRAM_NAME.to_owned()];
        argv.extend(["retention", "reclaim", "--all", "--format", "json"].map(str::to_owned));
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "retention reclaim --all sweeps every case and takes no --case. Drop --case, or name the \
             Artifacts of one case instead of --all; nothing was changed.",
        )
        .at("--case")
        .with_argv(argv));
    }
    let done = store.reclaim_withdrawn(apply)?;
    let root = store.workspace().root().to_owned();
    let swept: Vec<&DigestObligations> = done.iter().map(|done| &done.obligations).collect();
    let residual = workspace_copies(&root, &swept);
    let outside = exposure_cli::outside(store);
    let groups = grouped(&done, counted_view);
    let mut retry: Vec<String> = [PROGRAM_NAME, "retention", "reclaim", "--all", "--apply"]
        .map(str::to_owned)
        .to_vec();
    retry.extend(["--format".to_owned(), "json".to_owned()]);
    let would = done
        .iter()
        .any(|reclamation| reclamation.outcome == ReclaimOutcome::WouldReclaim);
    let next = if apply {
        after_reclaim(&done, retry)
    } else if would {
        json!([{
            "argv": retry,
            "kind": "local_write",
            "effect": "Removes the blobs listed as would_reclaim after rechecking each under the coordination lock; every record stays.",
        }])
    } else {
        after_reclaim(&done, {
            let mut again = retry;
            again.retain(|arg| arg != "--apply");
            again
        })
    };
    let notice = if apply {
        RECLAIM_NOTICE
    } else {
        SWEEP_PREVIEW_NOTICE
    };
    let mut lines = vec![format!(
        "retention reclaim --all  {}",
        if apply {
            "every case"
        } else {
            "every case  dry run: nothing was changed"
        }
    )];
    for reclamation in &done {
        let entry = &reclamation.obligations;
        lines.push(format!(
            "{:<16} {}  {} byte(s)",
            reclaim_word(reclamation.outcome),
            entry.digest,
            entry.size_bytes.unwrap_or_default()
        ));
    }
    if done.is_empty() {
        lines.push("no withdrawn reference records a digest; nothing to reclaim".to_owned());
    }
    copy_lines(&mut lines, "residual", &residual);
    lines.push(notice.to_owned());
    lines.push(format!("Next: {}", argv_line(&next)));
    let mut limitations: Vec<&str> = LIMITATIONS.to_vec();
    limitations.push(SWEEP_LIMITATION);
    let mut body = json!({
        "scope": "workspace",
        "dry_run": !apply,
        "writes": if apply { "blobs" } else { "nothing" },
        "residual": residual,
        "outside": outside,
        "limitations": limitations,
        "notice": notice,
        "next": next,
    });
    for (name, group) in groups {
        if apply && name == "would_reclaim" || !apply && name == "reclaimed" {
            continue;
        }
        body[name] = group;
    }
    Ok(Output::success(
        if apply {
            "retention_reclaimed"
        } else {
            "retention_reclaim_previewed"
        },
        body,
        lines.join("\n"),
    ))
}

/// One held digest with every holder counted and none named.
fn counted_view(entry: &DigestObligations) -> Value {
    json!({
        "digest": entry.digest,
        "size_bytes": entry.size_bytes,
        "counts": {
            "references": entry.references.len(),
            "method_packs": entry.method_packs.len(),
            "attachments": entry.attachments.len(),
        },
        "archive_pins": entry.archive_pins,
        "unsettled_staging": entry.unsettled_staging,
    })
}

/// `retention reclaim`: remove the stored bytes of withdrawn Artifacts of
/// the case that nothing else holds.
fn reclaim(store: &mut Store, global: &Global, artifacts: &[String]) -> Result<Output, Failure> {
    let case = select_case(store, global.case.as_deref())?;
    let replay = store.case(case)?;
    let ids = resolve_artifacts(&replay.state, case, artifacts)?;
    let losses = retention_ops::preview(&replay.state, case, &ids);
    for (index, loss) in losses.iter().enumerate() {
        if !retention_ops::availability_withdrawn(&replay.state, loss.artifact_ref.id) {
            let mut failure = Failure::new(
                ErrorCode::GateFailed,
                "Reclamation removes only bytes whose loss of availability is committed. Withdraw \
                 this Artifact's availability first with artifact.expire; nothing was changed.",
            )
            .at(format!("/artifacts/{index}"));
            failure.error.actual = Some(json!({
                "artifact_ref": loss.artifact_ref,
                "availability": loss.availability,
            }));
            return Err(failure.with_argv(vec![
                PROGRAM_NAME.to_owned(),
                "retention".to_owned(),
                "preview".to_owned(),
                loss.artifact_ref.id.to_string(),
                "--format".to_owned(),
                "json".to_owned(),
            ]));
        }
    }
    let digests: BTreeSet<Digest> = losses
        .iter()
        .filter_map(|loss| loss.digest.clone())
        .collect();
    let done = store.reclaim_blobs(&digests)?;
    let root = store.workspace().root().to_owned();
    let withdrawn: Vec<&ReferenceLoss> = losses.iter().collect();
    let residual = downstream(store, &replay.state, &root, &withdrawn);
    let outside = exposure_cli::outside(store);

    let groups = grouped(&done, |entry| held_view(case, entry));
    let group = |name: &str| groups[name].clone();
    let mut argv = vec![
        PROGRAM_NAME.to_owned(),
        "--case".to_owned(),
        case.to_string(),
        "retention".to_owned(),
        "reclaim".to_owned(),
    ];
    argv.extend(losses.iter().map(|loss| loss.artifact_ref.id.to_string()));
    argv.extend(["--format".to_owned(), "json".to_owned()]);
    let next = after_reclaim(&done, argv);

    let mut lines = vec![format!("retention reclaim  case {case}")];
    for reclamation in &done {
        let entry = &reclamation.obligations;
        lines.push(format!(
            "{:<16} {}  {} byte(s)",
            reclaim_word(reclamation.outcome),
            entry.digest,
            entry.size_bytes.unwrap_or_default()
        ));
    }
    copy_lines(&mut lines, "residual", &residual);
    lines.push(RECLAIM_NOTICE.to_owned());
    lines.push(format!("Next: {}", argv_line(&next)));
    Ok(Output::success(
        "retention_reclaimed",
        json!({
            "case_id": case,
            "references": losses
                .iter()
                .map(|loss| json!({
                    "artifact_ref": loss.artifact_ref,
                    "digest": loss.digest,
                    "size_bytes": loss.size_bytes,
                    "availability": loss.availability,
                }))
                .collect::<Vec<_>>(),
            "reclaimed": group("reclaimed"),
            "already_absent": group("already_absent"),
            "held_for_others": group("held_for_others"),
            "waiting": group("waiting"),
            "residual": residual,
            "outside": outside,
            "limitations": LIMITATIONS,
            "notice": RECLAIM_NOTICE,
            "next": next,
        }),
        lines.join("\n"),
    ))
}

const fn outcome_word(outcome: LossOutcome) -> &'static str {
    match outcome {
        LossOutcome::BecomesUnavailable => "becomes_unavailable",
        LossOutcome::AlreadyUnavailable => "already_unavailable",
        LossOutcome::Refused => "refused",
    }
}

fn reference_view(loss: &ReferenceLoss) -> Value {
    let mut value = json!(loss);
    value["evidence_read_after"] = json!(match loss.outcome {
        LossOutcome::BecomesUnavailable | LossOutcome::AlreadyUnavailable => "refused",
        LossOutcome::Refused => "unchanged",
    });
    value
}

/// One digest that stays stored because a record still holds it. Holders in
/// this case are named; holders in other cases are only counted.
fn held_view(case: Id, entry: &DigestObligations) -> Value {
    let same = |holder_case: Id| holder_case == case;
    json!({
        "digest": entry.digest,
        "size_bytes": entry.size_bytes,
        "references": entry
            .references
            .iter()
            .filter(|holder| same(holder.case_id))
            .map(|holder| holder.artifact_ref)
            .collect::<Vec<_>>(),
        "method_packs": entry
            .method_packs
            .iter()
            .filter(|holder| same(holder.case_id))
            .map(|holder| json!({"pack_id": holder.pack_id, "version": holder.version}))
            .collect::<Vec<_>>(),
        "attachments": entry
            .attachments
            .iter()
            .filter(|holder| same(holder.case_id))
            .map(|holder| holder.sequence)
            .collect::<Vec<_>>(),
        "other_cases": {
            "references": entry.references.iter().filter(|h| !same(h.case_id)).count(),
            "method_packs": entry.method_packs.iter().filter(|h| !same(h.case_id)).count(),
            "attachments": entry.attachments.iter().filter(|h| !same(h.case_id)).count(),
        },
        "archive_pins": entry.archive_pins,
        "unsettled_staging": entry.unsettled_staging,
    })
}

/// Known copies of the bytes of `releasing`, in a fixed order: records that
/// copy them (derivatives, quoting Observations), then workspace files.
/// A derivative whose own availability is already withdrawn is no longer a
/// readable copy and is left out.
fn downstream(
    store: &Store,
    state: &investigation_core::state::State,
    root: &Path,
    releasing: &[&ReferenceLoss],
) -> Vec<Value> {
    let mut copies = Vec::new();
    let mut files = BTreeMap::<PathBuf, (Area, &'static str)>::new();
    let vault = exposure_cli::Vault::load(store);
    let hexes: BTreeSet<String> = releasing
        .iter()
        .filter_map(|loss| loss.digest.as_ref())
        .filter_map(|digest| digest.as_str().strip_prefix("sha256:").map(str::to_owned))
        .collect();
    let mut quoted = false;
    for loss in releasing {
        let from = loss.artifact_ref;
        for derivative in loss
            .derivatives
            .iter()
            .filter(|derivative| !retention_ops::availability_withdrawn(state, derivative.id))
        {
            copies.push(json!({
                "kind": "derivative",
                "record": derivative,
                "from": from,
                "detail": "An Artifact derived from these bytes. It holds its own bytes under its own digest and keeps its own availability; withdraw it separately if it must stop being readable.",
            }));
        }
        for observation in &loss.quoting_observations {
            quoted = true;
            copies.push(json!({
                "kind": "quoted_text",
                "record": observation,
                "from": from,
                "detail": "Recorded quote text copied from these bytes into the Observation, its journal event and the index. A withdrawal leaves it in place; see investigation docs exposure.",
            }));
            for (path, area, _) in vault.of(observation.id) {
                if root.join(&path).is_file() {
                    files.insert(
                        PathBuf::from(vault.shown(&path)),
                        (area, "a rendered note that may show the quoted text"),
                    );
                }
            }
        }
    }
    for (path, hex) in exposure::digest_named_files(root) {
        if hexes.contains(&hex) && exposure::area(&path) == Area::Staging {
            files.insert(path, (Area::Staging, "a staged copy of the bytes"));
        }
    }
    for hex in &hexes {
        let verdict = verdict_path(hex);
        if root.join(&verdict).is_file() {
            files.insert(
                verdict,
                (
                    Area::BlobVerdict,
                    "names the digest and file identity of the bytes; holds no bytes",
                ),
            );
        }
    }
    if quoted {
        for path in exposure_cli::index_files(root) {
            files.insert(path, (Area::Index, "search text of the quoting records"));
        }
    }
    let sizes: BTreeMap<String, u64> = releasing
        .iter()
        .filter_map(|loss| {
            let hex = loss.digest.as_ref()?.as_str().strip_prefix("sha256:")?;
            Some((hex.to_owned(), loss.size_bytes?))
        })
        .collect();
    copies.extend(file_copies(root, files, &hexes, &sizes));
    copies
}

/// Workspace files that copy or name the swept digests: staged copies, blob
/// verdicts and exports. Names no record.
fn workspace_copies(root: &Path, swept: &[&DigestObligations]) -> Vec<Value> {
    let hexes: BTreeSet<String> = swept
        .iter()
        .filter_map(|entry| {
            entry
                .digest
                .as_str()
                .strip_prefix("sha256:")
                .map(str::to_owned)
        })
        .collect();
    let sizes: BTreeMap<String, u64> = swept
        .iter()
        .filter_map(|entry| {
            let hex = entry.digest.as_str().strip_prefix("sha256:")?;
            Some((hex.to_owned(), entry.size_bytes?))
        })
        .collect();
    let mut files = BTreeMap::new();
    for (path, hex) in exposure::digest_named_files(root) {
        if hexes.contains(&hex) && exposure::area(&path) == Area::Staging {
            files.insert(path, (Area::Staging, "a staged copy of the bytes"));
        }
    }
    for hex in &hexes {
        let verdict = verdict_path(hex);
        if root.join(&verdict).is_file() {
            files.insert(
                verdict,
                (
                    Area::BlobVerdict,
                    "names the digest and file identity of the bytes; holds no bytes",
                ),
            );
        }
    }
    file_copies(root, files, &hexes, &sizes)
}

/// `files` plus every export below exports/ that names one of `hexes` or
/// holds exactly the bytes of one, found by content among the files of a
/// recorded size (bounded by `exposure.copy_hash_*`), as residual entries.
fn file_copies(
    root: &Path,
    mut files: BTreeMap<PathBuf, (Area, &'static str)>,
    hexes: &BTreeSet<String>,
    sizes: &BTreeMap<String, u64>,
) -> Vec<Value> {
    let exports = Path::new("exports");
    let forms: Vec<Form> = hexes
        .iter()
        .map(|hex| Form {
            label: "digest",
            bytes: hex.clone().into_bytes(),
        })
        .collect();
    if !forms.is_empty() {
        let report = exposure::scan(&root.join(exports), &forms, &BTreeMap::new());
        for hit in report.hits {
            files.insert(
                exports.join(hit.path),
                (
                    Area::Export,
                    "an export that names the digest; it may hold a copy of the bytes",
                ),
            );
        }
    }
    let mut sought: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    for (hex, size) in sizes {
        sought.entry(*size).or_default().insert(hex.clone());
    }
    let found = exposure::copies_by_content(&root.join(exports), &sought);
    for (path, _) in found.copies {
        // A copy of the bytes outranks a file that only names the digest.
        files.insert(
            exports.join(path),
            (
                Area::Export,
                "an export file that holds exactly the bytes, found by content",
            ),
        );
    }
    let mut copies: Vec<Value> = files
        .into_iter()
        .map(|(path, (area, detail))| {
            json!({
                "kind": area.as_str(),
                "path": path.to_string_lossy(),
                "detail": detail,
            })
        })
        .collect();
    copies.extend(found.not_compared.into_iter().map(|unopened| {
        json!({
            "kind": Area::Export.as_str(),
            "path": exports.join(unopened.path).to_string_lossy(),
            "detail": format!("a file of a released size, {}; it may hold a copy of the bytes", unopened.reason),
        })
    }));
    copies
}
