//! `review-vault`: a browsable casebook for one audience, planned by the
//! vault renderer from the records that audience may see. Pure.
//!
//! ```text
//! DIR/
//!   Start Here.md                          what this is, its basis, how to read it
//!   Omitted Data.md                        what a review vault never holds, completeness
//!   Cases/<case>/Overview.md               the case Overview, as in the vault
//!   Cases/<case>/<Kind>/<record>.md        one note per shown record
//!   Cases/<case>/Views/<view>.md           static Markdown table views
//!   Attachments/<artifact>.<ext>           reviewed derivative previews, exact bytes
//!   Human Files.md                         human notes and boards shown records rely on
//!   human/sha256/<hex>                     their captured bytes, exact
//!   export-manifest.json                   written last
//! ```
//!
//! **Allowed inputs only.** The vault templates never see the working vault
//! or any record the audience may not see: the case is filtered through
//! [`ExportInput::includes`] first ([`crate::CaseSnapshot::filtered`]), and
//! the notes are planned from that filtered case. Nothing is read from
//! `vault/`, so stale generated files, conflict copies and the person-owned
//! folders (`Notes/`, `Boards/`, `Inbox/`) cannot appear as they are now.
//! Every channel question a template
//! asks (title, summary, content, metadata, lineage) is answered by the
//! export's [`super::Inclusion`], with no ID-only placeholders.
//!
//! **Filtering does not strengthen anything.** Effective states, objections,
//! freshness and the gap inventory are computed on the full pinned snapshot
//! ([`crate::note::ReviewScope`]): a conclusion whose objection is withheld
//! still reads `disputed`. A shown record carries the policy's fixed
//! [`super::Caveat`]s under its summary, wherever its note shows its standing.
//! A reference to a record not shown is left out of lists and replaced by
//! fixed words elsewhere; a gap whose rule read anything not shown is left
//! out; Pareto status is not listed when anything is withheld. Each such
//! gap and status, and each attachment not copied, is reported as an
//! [`super::Omission`], so the manifest, `Start Here.md` and
//! `Omitted Data.md` agree that the copy is incomplete.
//!
//! **Guard.** After planning, every file name and byte is scanned for every
//! withheld record's ID (prefixed, bare or 32-digit). A hit refuses the
//! export ([`RenderError::Withheld`]); nothing is written.
//!
//! **Attachments.** Only reviewed derivative Artifacts the audience may see
//! in full, not withdrawn, whose retained bytes the caller read and checked
//! against the recorded digest ([`super::Retained`]), become
//! `Attachments/<artifact>.<ext>`, byte for byte. Originals, manifests,
//! unreviewed derivatives and raw captures never do. A public export
//! rewrites identifiers in every planned file, and a rewritten attachment
//! would no longer be the recorded evidence, so an attachment whose bytes
//! hold any UUIDv7, any of the case's record IDs or its head hash is not
//! copied at all, for every audience: `Omitted Data.md` says so beside a
//! link to its note. Every copied attachment therefore passes any
//! identifier rewrite unchanged and keeps its recorded digest.
//!
//! **Human files.** A person-owned note or board a record relies on
//! appears only through its snapshot annotation (W11.14), and only when the
//! annotation, its Artifact and that Artifact's Source are all included
//! ([`super::human_files`], as for a case report). `Human Files.md` lists
//! each with its vault path, role, digest and size; the captured bytes, never
//! the live file, are copied to `human/sha256/<hex>`. As for attachments,
//! bytes that may hold an identifier are never copied, for every audience
//! (the case report's rule copies them unless identifiers are remapped), so
//! no copy can name a withheld record or be altered by a remap; each one
//! left out is listed with the reason and reported as an
//! [`super::Omission`]. Withdrawn or reclaimed bytes are never read.
//!
//! **Caveat columns.** A view table whose rows include a caveated record
//! gains an `Audience caveat` column holding the caveats word for word, as
//! well as the per-note caveat section ([`crate::records::views`]).
//!
//! **Identifiers.** The notes name canonical IDs and the framework remaps
//! them for an export whose identifiers are remapped (W11.13). Such an
//! export also shows no journal position: no acceptance sequence, or earlier
//! snapshot's sequence and head in any note or view.
//!
//! **Record times.** Times the workspace stamped on records (recorded,
//! acquired) follow [`super::RecordTimes`]: exact when canonical, the UTC
//! day when remapped, none for public (bn-18la).
//!
//! The vault's optional viewer features (Bases, Canvas) are never planned:
//! the review vault is plain Markdown with static tables, readable on a
//! phone or offline without plugins or a shell. It is not resumable: it
//! holds no journal, event, original, configuration or cache.

use std::{collections::BTreeSet, fmt::Write as _};

use investigation_core::{
    classification::Field,
    derivation,
    evidence::ArtifactRole,
    identity::{Id, Kind},
    source_ops::EvidentiaryStatus,
    state::{Record, State},
    wire::{Digest, Snapshot},
};

use super::{
    ExportInput, ExportRenderer, ExportSnapshot, Inclusion, Omission, OmissionReason, Omitted,
    Statements, dependent_rule, human_files,
};
use crate::{
    Audience, Phase, PlanBuilder, PlanPath, RenderError, VerifiedSnapshot, ViewPolicy, layout,
    marker,
    note::{NoteContext, REVIEW_BANNER, ReviewScope, VAULT_SCHEMA},
    plan::plan_case,
    text::{inline_code, table_code},
    yaml::Frontmatter,
};

/// Output format of this kind. It plans the vault templates, so a change
/// to the vault renderer's bytes (`RENDERER_VERSION`) changes it too; the
/// manifest names both through this value and the snapshot.
/// v2 (W11.16): every note, not only the views and the Overview, carries
/// the caveats of the caveated records it links, since a record note's
/// backlinks show their standing too.
/// v3 (bn-1vg8): completeness counts gaps, Pareto status and attachments
/// left out, with the framework's completeness statement.
/// v4 (bn-91gz): a Retrospective step or lesson citing a record not shown
/// is left out of its note and the Views, and reported.
/// v5 (bn-48uc): selected human-note snapshots (`Human Files.md` and their
/// captured bytes under `human/sha256/`), and an `Audience caveat` column in
/// view tables whose rows include a caveated record.
/// v6 (bn-18la): record times (recorded, acquired) follow the export's
/// record-time policy: exact when canonical, the UTC day when remapped, none
/// for public; acquisition times were shown exactly before.
pub const VERSION: &str = "inv.export.review-vault.v6";

/// The vault renderer version whose templates this kind plans.
pub const TEMPLATES: &str = crate::plan::RENDERER_VERSION;

/// The entry note.
pub const ENTRY_FILE: &str = "Start Here.md";

/// The omitted-data note.
pub const OMITTED_FILE: &str = "Omitted Data.md";

/// The note listing the human files shown records rely on.
pub const HUMAN_FILE: &str = "Human Files.md";

pub struct ReviewVault;

impl ExportRenderer for ReviewVault {
    fn version(&self) -> &'static str {
        VERSION
    }

    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError> {
        let _links = layout::LinkRoot::enter(None);
        let full = input.case;
        let case_id = full.case_id();
        let included: BTreeSet<Id> = full
            .state()
            .records()
            .filter(|record| input.includes(record))
            .map(|record| record.header.id)
            .collect();
        // The notes name the snapshot the export names: the case head, or
        // the bundle (sequence 0) when identifiers are remapped.
        let shown = input.snapshot();
        let head = Snapshot {
            sequence: shown.sequence,
            head_hash: shown.head_hash.clone(),
        };
        let filtered = full.filtered(|id| included.contains(&id), head);
        let snapshot = VerifiedSnapshot::of_case(files.generation().workspace_id(), filtered);
        let case = snapshot
            .case(case_id)
            .expect("the filtered snapshot holds its case");
        let policy = ViewPolicy::for_audience(input.audience);
        let shows = |record: &Record, field: Field| input.inclusion.may_show(record, field);
        let caveats = |record: &Record| -> Vec<&'static str> {
            input
                .caveats(record)
                .iter()
                .map(|caveat| caveat.text())
                .collect()
        };
        let omit = |content: Omitted, record: Option<Id>, part: Option<&str>, rests: &[Id]| {
            // Not shown here means withheld, or held at a channel this
            // audience may not see.
            let reason = input
                .rests_on(rests.iter().copied())
                .unwrap_or(OmissionReason::RestsOnWithheld);
            let mut omission = Omission::new(content, reason);
            omission.record = record;
            omission.part = part.map(str::to_owned);
            input.omit(omission);
        };
        let generation = files.generation().clone();
        let notes = NoteContext::for_review(
            case,
            ReviewScope {
                standing: full,
                shows: &shows,
                caveats: &caveats,
                positions: !input.remapped(),
                times: input.record_times(),
                omit: &omit,
            },
            &policy,
            &generation,
        );

        let mut cases = PlanBuilder::new(generation.clone());
        plan_case(&notes, &mut cases)?;
        let mut plan = PlanBuilder::new(generation.clone());
        for entry in cases.finish().entries() {
            let mut content = entry.content().to_vec();
            if entry.path().as_str().ends_with(".md") {
                caveat_section(&mut content, &notes, entry.projects())?;
            }
            plan.add(
                entry.phase(),
                entry.path().clone(),
                content,
                entry.projects(),
            )?;
        }
        // Every omission is reported before the two notes that state
        // completeness are planned.
        let attachments = attachments(input, case.state());
        let mut copies = PlanBuilder::new(generation.clone());
        let human = human_files::plan_with(input, &mut copies, true)?;
        if !human.is_empty() {
            plan.add(
                Phase::Views,
                PlanPath::new(HUMAN_FILE)?,
                human_note(input, &human),
                None,
            )?;
        }
        plan.add(
            Phase::Entry,
            layout::start_here()?,
            entry_note(input, &notes, &attachments, !human.is_empty())?,
            None,
        )?;
        plan.add(
            Phase::Views,
            PlanPath::new(OMITTED_FILE)?,
            omitted_note(input, &notes, &attachments)?,
            None,
        )?;
        let plan = marker::stamp_plan(plan.finish(), &snapshot)?;

        let withheld = withheld_forms(input.inclusion, case_id);
        for entry in plan.entries() {
            guard(entry.path().as_str(), entry.content(), &withheld)?;
            files.add(
                entry.phase(),
                entry.path().clone(),
                entry.content().to_vec(),
                entry.projects(),
            )?;
        }
        for copy in copies.finish().entries() {
            guard(copy.path().as_str(), copy.content(), &withheld)?;
            files.add(
                copy.phase(),
                copy.path().clone(),
                copy.content().to_vec(),
                None,
            )?;
        }
        for attachment in attachments.copied {
            guard(attachment.path.as_str(), &attachment.bytes, &withheld)?;
            input.copied_verbatim(attachment.path.as_str(), attachment.digest.clone());
            files.add(
                Phase::Records,
                attachment.path,
                attachment.bytes,
                Some(attachment.artifact),
            )?;
        }
        Ok(())
    }
}

/// Append the fixed caveats of every caveated record a note links, so a
/// reader who sees its standing in a table row, a list or a backlink sees
/// them too. A record's own note shows its caveats under its summary.
fn caveat_section(
    content: &mut Vec<u8>,
    notes: &NoteContext<'_>,
    own: Option<Id>,
) -> Result<(), RenderError> {
    let case = notes.case();
    let text = String::from_utf8_lossy(content).into_owned();
    let mut section = String::new();
    for record in case.state().records() {
        let caveats = notes.audience_caveats(record);
        if caveats.is_empty() || own == Some(record.header.id) {
            continue;
        }
        let Some(path) = notes.note_path(record.header.id)? else {
            continue;
        };
        let target = format!("[[{}", path.as_str().trim_end_matches(".md"));
        let linked = text.match_indices(target.as_str()).any(|(at, _)| {
            matches!(
                text.as_bytes().get(at + target.len()),
                Some(b'|' | b'\\' | b']' | b'#')
            )
        });
        if !linked {
            continue;
        }
        let _ = writeln!(section, "- {}", notes.link(record.header.id)?);
        for caveat in caveats {
            let _ = writeln!(section, "  - Audience caveat: {caveat}");
        }
    }
    if !section.is_empty() {
        let mut out = String::from(
            "\n## Audience caveats\n\nRecords linked above whose standing must be read with the \
             audience policy's fixed caveats:\n\n",
        );
        out.push_str(&section);
        content.extend_from_slice(out.as_bytes());
    }
    Ok(())
}

/// The digests of the retained bytes a review vault of `snapshot` for
/// `audience` may copy: reviewed derivatives the audience may see in full.
/// The caller reads exactly these, checked against their digests, and hands
/// them to [`super::plan_export_with`]; nothing else is opened.
pub fn attachment_digests(snapshot: &ExportSnapshot, audience: Audience) -> Vec<Digest> {
    let state = snapshot.case().state();
    let inclusion = Inclusion::new(state, snapshot.today(), audience);
    let mut digests: Vec<Digest> = state
        .records_of_kind(Kind::Artifact)
        .filter(|record| !inclusion.availability_withdrawn(record.header.id))
        .filter_map(|record| candidate(state, &inclusion, record))
        .collect();
    digests.sort();
    digests.dedup();
    digests
}

/// The recorded digest of `record` when it is a derivative a review vault
/// may attach: shown in full, reviewed, not withdrawn, retained.
fn candidate(state: &State, inclusion: &Inclusion<'_>, record: &Record) -> Option<Digest> {
    if record.header.id.kind() != Kind::Artifact || !inclusion.may_show(record, Field::Content) {
        return None;
    }
    let body = derivation::artifact_body(state, record.reference())?;
    let reviewed = body
        .derivation
        .as_ref()
        .is_some_and(|derivation| derivation.review.is_some());
    (body.role() == ArtifactRole::Derivative
        && reviewed
        && body.status != Some(EvidentiaryStatus::Withdrawn)
        && body.availability == investigation_core::evidence::ArtifactAvailability::Retained)
        .then_some(body.digest)
        .flatten()
}

struct Attachment {
    artifact: Id,
    path: PlanPath,
    bytes: Vec<u8>,
    digest: Digest,
}

/// What the attachment pass decided, by artifact ID.
#[derive(Default)]
struct Attachments {
    copied: Vec<Attachment>,
    /// Shown reviewed derivatives that are not copied, and why.
    skipped: Vec<(Id, &'static str)>,
}

/// Plan the attachments of the shown records in `shown`.
fn attachments(input: &ExportInput<'_>, shown: &State) -> Attachments {
    let full = input.case.state();
    let identifiers = case_identifiers(input);
    let mut found = Attachments::default();
    for record in shown.records_of_kind(Kind::Artifact) {
        let Some(digest) = candidate(full, input.inclusion, record) else {
            continue;
        };
        let id = record.header.id;
        if input.inclusion.availability_withdrawn(id) {
            found.skipped.push((
                id,
                "its availability was withdrawn (artifact.expire), so no bytes are read or \
                 copied through it",
            ));
            continue;
        }
        let Some(bytes) = input.retained.get(&digest) else {
            input.omit(Omission::new(Omitted::Attachment, OmissionReason::BytesNotRead).of(id));
            found
                .skipped
                .push((id, "its retained bytes were not read for this export"));
            continue;
        };
        if holds_identifiers(bytes, &identifiers) {
            input.omit(
                Omission::new(Omitted::Attachment, OmissionReason::BytesHoldIdentifiers).of(id),
            );
            found.skipped.push((
                id,
                "its bytes hold record identifiers or a journal head; an export may rewrite \
                 those, and a rewritten copy would not be the recorded evidence",
            ));
            continue;
        }
        let format = derivation::artifact_body(full, record.reference())
            .and_then(|body| body.format)
            .unwrap_or_default();
        let Ok(path) = layout::attachment(id, extension(&format)) else {
            continue;
        };
        found.copied.push(Attachment {
            artifact: id,
            path,
            bytes: bytes.to_vec(),
            digest,
        });
    }
    found
}

/// A short, inert file extension for a recorded media type. Markdown and
/// HTML travel as text, so a viewer never renders evidence as a note or a
/// page.
fn extension(format: &str) -> &'static str {
    let format = format.to_ascii_lowercase();
    let essence = format.split(';').next().unwrap_or_default().trim();
    match essence {
        "application/json" => "json",
        "text/csv" => "csv",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "application/pdf" => "pdf",
        _ if essence.starts_with("text/") => "txt",
        _ => "bin",
    }
}

/// What a copied attachment must not hold: the 32-digit form of every
/// record ID of the case, and the hex of its head hash.
struct Identifiers {
    simple: BTreeSet<String>,
    head: String,
}

fn case_identifiers(input: &ExportInput<'_>) -> Identifiers {
    Identifiers {
        simple: input
            .case
            .state()
            .records()
            .map(|record| record.header.id.uuid().simple().to_string())
            .collect(),
        head: hex(&input.case.head().head_hash).to_owned(),
    }
}

fn hex(digest: &Digest) -> &str {
    digest.as_str().trim_start_matches("sha256:")
}

/// Whether `bytes` hold any hyphenated UUIDv7 (any case, any boundary), a
/// case record ID in its 32-digit form or the case head hash.
fn holds_identifiers(bytes: &[u8], identifiers: &Identifiers) -> bool {
    if bytes.windows(36).any(is_uuid7) {
        return true;
    }
    let lower = bytes.to_ascii_lowercase();
    let text = String::from_utf8_lossy(&lower);
    text.contains(identifiers.head.as_str())
        || hex_runs(&lower, 32).any(|run| identifiers.simple.contains(run))
}

fn is_uuid7(bytes: &[u8]) -> bool {
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'7',
            19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Runs of exactly `length` hex digits in lowercase `bytes`, bounded by
/// anything that is not a hex digit.
fn hex_runs(bytes: &[u8], length: usize) -> impl Iterator<Item = &str> {
    bytes
        .split(|byte| !byte.is_ascii_hexdigit())
        .filter(move |run| run.len() == length)
        .filter_map(|run| std::str::from_utf8(run).ok())
}

/// Every form a withheld record's ID can take in a file: `obs_<uuid>`, the
/// bare UUID and its 32 digits, lowercase. The case's own ID is the export's
/// subject and is excepted.
fn withheld_forms(inclusion: &Inclusion<'_>, case_id: Id) -> Vec<String> {
    let mut forms = Vec::new();
    for exclusion in inclusion.exclusions() {
        if exclusion.id == case_id {
            continue;
        }
        let uuid = exclusion.id.uuid();
        forms.push(uuid.hyphenated().to_string());
        forms.push(uuid.simple().to_string());
    }
    forms
}

/// Refuse a planned file that names a withheld record in its path or bytes.
fn guard(path: &str, bytes: &[u8], withheld: &[String]) -> Result<(), RenderError> {
    if withheld.is_empty() {
        return Ok(());
    }
    let lower = format!("{path}\n{}", String::from_utf8_lossy(bytes)).to_ascii_lowercase();
    if withheld.iter().any(|form| lower.contains(form.as_str())) {
        return Err(RenderError::Withheld {
            path: path.to_owned(),
        });
    }
    Ok(())
}

/// `Start Here.md`: what this folder is, its basis, and how to read it.
fn entry_note(
    input: &ExportInput<'_>,
    notes: &NoteContext<'_>,
    attachments: &Attachments,
    human_files: bool,
) -> Result<String, RenderError> {
    let case = notes.case();
    let case_id = case.case_id();
    let head = case.head();
    let overview = layout::case_overview(case_id)?;
    let title = case
        .state()
        .current(case_id)
        .and_then(|record| notes.title(record));
    let mut out = Frontmatter::new()
        .text("inv_schema", VAULT_SCHEMA)
        .text("inv_export_kind", input.kind.name)
        .text("inv_audience", input.audience.as_str())
        .list("tags", &["investigation/review"])
        .finish();
    let _ = write!(
        out,
        "# Review copy: {}\n\n{REVIEW_BANNER}\n",
        title.as_deref().unwrap_or("one case")
    );
    let _ = writeln!(
        out,
        "This folder is a review copy of one case, exported by Investigation for the {} \
         audience from one verified snapshot. It shows only what that audience may see. It is \
         not the investigation: it holds no journal, event, original evidence, configuration \
         or local state, so it cannot resume or restore the case. A recovery archive is a \
         different export.",
        inline_code(input.audience.as_str())
    );
    out.push_str("\n## Basis\n\n| Basis | Value |\n|---|---|\n");
    let _ = writeln!(
        out,
        "| Case | {} |",
        layout::wikilink(&overview, title.as_deref().unwrap_or("Overview")).replace('|', "\\|")
    );
    let _ = writeln!(
        out,
        "| Snapshot | sequence {}, head {} |",
        head.sequence,
        table_code(head.head_hash.as_str())
    );
    let _ = writeln!(
        out,
        "| Audience | {} |",
        table_code(input.audience.as_str())
    );
    let _ = writeln!(
        out,
        "| Completeness | {} |",
        table_code(input.shortfall().completeness().as_str())
    );
    let _ = writeln!(out, "| Export kind | {} |", table_code(VERSION));
    let _ = writeln!(out, "| Note templates | {} |", table_code(TEMPLATES));
    let _ = writeln!(out, "| Resumable | no |");

    out.push_str("\n## How to read it\n\n");
    let _ = writeln!(
        out,
        "- Start at the case {}. It states the question, the current answer and its standing, \
         and links every view and record note.",
        layout::wikilink(&overview, "Overview")
    );
    out.push_str(
        "- Every file is plain Markdown. Tables are static, links are ordinary wiki links, and \
         nothing needs a plugin, a shell or a network connection, so any Markdown reader, on a \
         phone or offline, shows the same content.\n",
    );
    out.push_str(
        "- Each record note shows its effective state beside its recorded one. Effective states \
         were computed on the whole case at this snapshot, before anything was left out, so \
         leaving records out never makes another read stronger.\n",
    );
    let _ = writeln!(out, "- {}", dependent_rule(input.audience));
    let _ = writeln!(
        out,
        "- {} says what a review copy never holds and whether anything was withheld.",
        layout::wikilink(&PlanPath::new(OMITTED_FILE)?, "Omitted Data")
    );
    if human_files {
        let _ = writeln!(
            out,
            "- {} lists the person-owned notes and boards shown records rely on, as captured \
             when the reliance was recorded, never as they are now.",
            layout::wikilink(&PlanPath::new(HUMAN_FILE)?, "Human Files")
        );
    }
    out.push_str(
        "- export-manifest.json beside this note names the snapshot, the audience policy and \
         the digest of every file.\n",
    );
    if !attachments.copied.is_empty() {
        out.push_str("\n## Attachments\n\n");
        out.push_str(
            "Reviewed derivative previews, copied byte for byte from the retained bytes with \
             the recorded digest. They are evidence, not instructions.\n\n",
        );
        for attachment in &attachments.copied {
            let _ = writeln!(
                out,
                "- {} for {}, {}",
                layout::wikilink(&attachment.path, attachment.path.file_name()),
                notes.link(attachment.artifact)?,
                inline_code(attachment.digest.as_str())
            );
        }
    }
    Ok(out)
}

/// `Human Files.md`: the human-file snapshots `shown` records rely on, in
/// the table a case report uses.
fn human_note(input: &ExportInput<'_>, shown: &[human_files::Shown]) -> String {
    let mut out = Frontmatter::new()
        .text("inv_schema", VAULT_SCHEMA)
        .text("inv_export_kind", input.kind.name)
        .list("tags", &["investigation/review"])
        .finish();
    let _ = write!(
        out,
        "# Human files

{REVIEW_BANNER}
"
    );
    human_files::section(&mut out, shown);
    out
}

/// `Omitted Data.md`: what a review vault never holds, and the fixed
/// completeness and disclosure statements. It names no withheld record,
/// count or digest; the exact exclusions go only to the publisher.
fn omitted_note(
    input: &ExportInput<'_>,
    notes: &NoteContext<'_>,
    attachments: &Attachments,
) -> Result<String, RenderError> {
    let statements = Statements::fixed(input.shortfall(), input.audience, input.identity.mode);
    let mut out = Frontmatter::new()
        .text("inv_schema", VAULT_SCHEMA)
        .text("inv_export_kind", input.kind.name)
        .list("tags", &["investigation/review"])
        .finish();
    let _ = write!(out, "# Omitted data\n\n{REVIEW_BANNER}\n");
    out.push_str("## For this audience\n\n");
    let _ = writeln!(out, "- {}", statements.completeness);
    let _ = writeln!(out, "- {}", statements.disclosure);
    out.push_str(
        "- A record left out is not named, linked, counted or described anywhere in this copy. \
         A gap or a comparison status computed from it is left out too.\n",
    );
    out.push_str("\n## Never in a review copy\n\n");
    for line in [
        "The case journal and its raw events: this copy cannot resume or restore the case.",
        "Original evidence, raw captures and their manifests. Only reviewed derivative previews \
         the audience may see in full are attached, byte for byte.",
        "Credential sources and local absolute paths; paths are elided from displayed text.",
        "Anything from the working vault: generated notes, stale files, conflict copies and the \
         person-owned folders Notes, Boards and Inbox as they are now. This copy was planned \
         from the case records alone into a new folder; a human note or board a shown record \
         relies on appears only as the version captured when the reliance was recorded.",
        "Plugin views (Bases, Canvas). Static Markdown tables show the same views.",
        "The configuration, caches and machine-local state of the workspace.",
    ] {
        let _ = writeln!(out, "- {line}");
    }
    if !attachments.skipped.is_empty() {
        out.push_str("\n## Attachments not copied\n\n");
        for (artifact, reason) in &attachments.skipped {
            let _ = writeln!(out, "- {}: {reason}.", notes.link(*artifact)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_inert_and_short() {
        assert_eq!(extension("text/markdown"), "txt");
        assert_eq!(extension("text/html; charset=utf-8"), "txt");
        assert_eq!(extension("application/json"), "json");
        assert_eq!(extension("IMAGE/PNG"), "png");
        assert_eq!(extension("application/x-sh"), "bin");
        assert_eq!(extension(""), "bin");
    }

    #[test]
    fn identifiers_in_attachment_bytes_are_found_in_every_form() {
        let identifiers = Identifiers {
            simple: BTreeSet::from(["018d87fc52347000800000000000000a".to_owned()]),
            head: "ab".repeat(32),
        };
        for text in [
            "see obs_018D87FC-5234-7000-8000-00000000000A here",
            "x018d87fc-5234-7abc-9000-00000000000ay",
            "id 018d87fc52347000800000000000000a.",
            &format!("head {}", "AB".repeat(32)),
        ] {
            assert!(holds_identifiers(text.as_bytes(), &identifiers), "{text}");
        }
        for text in [
            "Limit 30 ms",
            "018d87fc-5234-4000-8000-00000000000a is a v4 UUID",
            "018d87fc52347000800000000000000b",
        ] {
            assert!(!holds_identifiers(text.as_bytes(), &identifiers), "{text}");
        }
    }

    #[test]
    fn the_guard_finds_a_withheld_id_in_a_path_or_body_in_any_case() {
        let withheld = vec![
            "018d87fc-5234-7000-8000-00000000000a".to_owned(),
            "018d87fc52347000800000000000000a".to_owned(),
        ];
        assert!(guard("a.md", b"clean", &withheld).is_ok());
        assert!(guard("a.md", b"clean", &[]).is_ok());
        for (path, body) in [
            ("Cases/c/obs_018d87fc-5234-7000-8000-00000000000a.md", "x"),
            ("a.md", "OBS_018D87FC-5234-7000-8000-00000000000A"),
            ("a.md", "digits 018d87fc52347000800000000000000a"),
        ] {
            assert!(matches!(
                guard(path, body.as_bytes(), &withheld),
                Err(RenderError::Withheld { .. })
            ));
        }
    }
}
