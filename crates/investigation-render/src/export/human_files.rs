//! Person-owned files a record relies on, in a report export or a review
//! copy: the captured version, never the live one (plan §12.1, W11.14,
//! bn-48uc). Pure.
//!
//! A record relies on a human note or board through a snapshot annotation
//! ([`NoteSnapshot`]): its artifact content holds the bytes captured when the
//! reliance was recorded. A report lists each snapshot the audience may see
//! with its vault path, role, owner, digest and size, and copies the
//! captured bytes beside the report under `human/sha256/<hex>`, the layout a
//! workspace archive uses ([`super::archive::human_member`]). The live file
//! in the vault is never read while planning, so a later edit cannot change
//! what an export of the same snapshot says the record relied on.
//!
//! A snapshot appears only when the annotation, its Artifact and that
//! Artifact's Source are all included for the audience, and the subjects it
//! names are listed only when included too. A note captured at `restricted`
//! therefore never reaches an `internal` or `public` export: not its bytes,
//! its path, its digest or its count.
//!
//! A remapped export (always for `public`, W11.13) rewrites every identifier
//! in every file it writes. Captured bytes that may hold one are therefore
//! listed but not copied, so a copy is always exactly the captured bytes,
//! and each one is reported as an [`super::Omission`] (bn-1vg8).

use std::{collections::BTreeSet, fmt::Write as _};

use investigation_core::{
    command::RevisionRef, derivation::artifact_body, human_file::NoteSnapshot, wire::Digest,
};

use super::{ExportInput, Omission, OmissionReason, Omitted, archive::human_member};
use crate::{Phase, PlanBuilder, PlanPath, RenderError, text::inline_code};

/// Why a shown snapshot's bytes are not copied.
const NOT_RETAINED: &str = "bytes not retained";
/// A review copy copies only bytes that hold no identifier.
const HOLDS_ANY_IDENTIFIER: &str = "not copied: the captured bytes hold identifiers, and a review copy copies only bytes that hold none";
/// The captured bytes' Artifact had its availability withdrawn.
const WITHDRAWN: &str = "availability withdrawn: no bytes are copied through this reference";
/// A remapped export rewrites every identifier in every file body, so a copy
/// of bytes holding one would differ from the captured bytes it claims to be.
const HOLDS_IDENTIFIERS: &str =
    "not copied: the captured bytes hold identifiers that this export remaps";

/// One snapshot shown in an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    pub snapshot: NoteSnapshot,
    /// The export member holding the captured bytes, or why there is none.
    pub member: Result<String, &'static str>,
    /// Subjects the audience may see.
    pub subjects: Vec<RevisionRef>,
}

/// The snapshots `input` may show, with their captured bytes added to
/// `files`, in annotation ID order.
pub(crate) fn plan(
    input: &ExportInput<'_>,
    files: &mut PlanBuilder,
) -> Result<Vec<Shown>, RenderError> {
    plan_with(input, files, false)
}

/// [`plan`]; with `exact_only`, bytes that may hold an identifier are left
/// out for every audience, not only when identifiers are remapped: listed,
/// not copied, and reported. A review copy uses it, as it does for its
/// attachments: it refuses any file naming a withheld record, and a copy
/// without identifiers can name none, so whether a copy was left out never
/// depends on what is withheld.
pub(crate) fn plan_with(
    input: &ExportInput<'_>,
    files: &mut PlanBuilder,
    exact_only: bool,
) -> Result<Vec<Shown>, RenderError> {
    let state = input.case.state();
    let visible = |reference: RevisionRef| {
        state
            .revision(reference)
            .is_some_and(|record| input.includes(record))
            && state
                .current(reference.id)
                .is_some_and(|record| input.includes(record))
    };
    let mut copied = BTreeSet::<Digest>::new();
    let mut shown = Vec::new();
    for snapshot in NoteSnapshot::all(state) {
        let Some(annotation) = state.revision(snapshot.annotation) else {
            continue;
        };
        let source = artifact_body(state, snapshot.artifact).map(|body| body.source);
        if !input.includes(annotation)
            || !visible(snapshot.artifact)
            || !source.is_some_and(visible)
        {
            continue;
        }
        let withdrawn = input.inclusion.availability_withdrawn(snapshot.artifact.id);
        let member = match input.retained.get(&snapshot.sha256) {
            _ if withdrawn => Err(WITHDRAWN),
            Some(bytes) if input.remapped() && may_hold_identifier(bytes) => {
                input.omit(
                    Omission::new(Omitted::HumanFileBytes, OmissionReason::IdentifiersRemapped)
                        .of(snapshot.annotation.id),
                );
                Err(HOLDS_IDENTIFIERS)
            }
            Some(bytes) if exact_only && may_hold_identifier(bytes) => {
                input.omit(
                    Omission::new(
                        Omitted::HumanFileBytes,
                        OmissionReason::BytesHoldIdentifiers,
                    )
                    .of(snapshot.annotation.id),
                );
                Err(HOLDS_ANY_IDENTIFIER)
            }
            Some(bytes) => {
                let member = human_member(&snapshot.sha256);
                if copied.insert(snapshot.sha256.clone()) {
                    files.add(
                        Phase::Records,
                        PlanPath::new(member.clone())?,
                        bytes.to_vec(),
                        None,
                    )?;
                }
                Ok(member)
            }
            None => Err(NOT_RETAINED),
        };
        let subjects = snapshot
            .subjects
            .iter()
            .copied()
            .filter(|subject| visible(*subject))
            .collect();
        shown.push(Shown {
            snapshot,
            member,
            subjects,
        });
    }
    Ok(shown)
}

/// The report section listing `shown`.
pub(crate) fn section(out: &mut String, shown: &[Shown]) {
    out.push_str("\n## Human files relied on\n\n");
    if shown.is_empty() {
        out.push_str("- None recorded or included.\n");
        return;
    }
    out.push_str(
        "Person-owned vault files that records rely on, as captured when the reliance was \
         recorded. Each copy is the captured version, not the file as it is now: a later edit \
         to the note does not change it. A snapshot is authored text with no evidentiary \
         standing.\n\n| File | Role | Owner | SHA-256 | Bytes | Copy | Annotation | Relied on by |\n\
         |---|---|---|---|---|---|---|---|\n",
    );
    for item in shown {
        let snapshot = &item.snapshot;
        let subjects = if item.subjects.is_empty() {
            "none included".to_owned()
        } else {
            item.subjects
                .iter()
                .map(|subject| inline_code(&subject.id.to_string()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {subjects} |",
            inline_code(&snapshot.file.path),
            inline_code(snapshot.role()),
            inline_code(&snapshot.file.owner),
            inline_code(snapshot.sha256.as_str()),
            snapshot
                .bytes
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
            item.member
                .as_deref()
                .map_or_else(|reason| (*reason).to_owned(), inline_code),
            inline_code(&snapshot.annotation.id.to_string()),
        );
    }
}

/// Whether `bytes` may hold an identifier a remapped export rewrites: a
/// hyphenated UUID of any version, or a run of 32 or more hex digits (an ID
/// in its 32-digit form, or a head hash). Deliberately broader than the
/// remapper, so a copy is never altered by it.
fn may_hold_identifier(bytes: &[u8]) -> bool {
    let mut run = 0usize;
    for byte in bytes {
        run = if byte.is_ascii_hexdigit() { run + 1 } else { 0 };
        if run >= 32 {
            return true;
        }
    }
    bytes.windows(36).any(|window| {
        window.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::may_hold_identifier;

    #[test]
    fn identifiers_in_captured_bytes_are_found_in_every_form() {
        for holds in [
            "see obs_019acd16-0003-7000-8000-000000000001 here",
            "UPPER 019ACD16-0003-7000-8000-000000000001",
            "a v4 one 550e8400-e29b-41d4-a716-446655440000",
            "bare 019acd16000370008000000000000001",
            "a head hash 5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03",
        ] {
            assert!(may_hold_identifier(holds.as_bytes()), "{holds}");
        }
        for plain in [
            "# Plan\n\nThe proxy timeout is 30 s.\n",
            "short hex deadbeef and a date 2026-11-23",
            "",
        ] {
            assert!(!may_hold_identifier(plain.as_bytes()), "{plain}");
        }
    }
}
