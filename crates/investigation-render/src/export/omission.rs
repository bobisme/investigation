//! Omissions: content a kind leaves out of an export beyond what record
//! inclusion withholds (bn-1vg8). Pure.
//!
//! [`super::Inclusion`] decides records. A kind also shows content that is
//! not a record of its own: a lesson or step of a Retrospective, a matrix
//! row, cell or Pareto status, a gap, a finding, a next step, a flag's
//! reason, an attachment or retained bytes. When a kind leaves such content
//! out, because what it rests on is withheld or not held in the snapshot, or
//! because its bytes cannot be copied exactly, it reports an [`Omission`]
//! through [`super::ExportInput::omit`] (or [`super::ExportInput::admits`],
//! which decides and reports in one call). Any omission makes the export
//! `incomplete`, exactly as an [`super::Exclusion`] does, so an export that
//! left anything out never reads complete.
//!
//! Disclosure follows exclusions. The exact omissions go only to the
//! publisher ([`super::ExportPlan::omissions`], the command's receipt). The
//! export itself carries only `completeness` and one fixed sentence chosen by
//! [`Shortfall`], the same for every audience:
//!
//! | Withheld records | Omissions | `completeness` | Statement |
//! |---|---|---|---|
//! | none | none | `complete` | nothing was withheld |
//! | some | any | `incomplete` | the policy withheld material; content may be left out too |
//! | none | some | `incomplete` | nothing was withheld, but content was left out |
//!
//! When records are withheld the statement does not say whether any
//! omission happened as well: an omission that rests on withheld material
//! would otherwise tell the audience that some content it sees, or would
//! have seen, depends on what it may not see. The statement for omissions
//! alone tells it only what `complete` told it before: that nothing was
//! withheld. No statement names, counts or describes what was left out.
//!
//! Listing limits (a list cut at a fixed length with "and N more") are not
//! omissions: the file states them where they apply.

use std::{cell::RefCell, collections::BTreeSet};

use investigation_core::identity::Id;
use serde::Serialize;

use super::Completeness;

/// What a kind left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Omitted {
    /// A Retrospective's proposed lesson.
    Lesson,
    /// A Retrospective's effective or invalid step.
    Step,
    /// A comparison matrix row (an option) that is shown elsewhere.
    MatrixRow,
    /// A comparison matrix cell, shown as "not shown".
    MatrixCell,
    /// The Pareto status of a comparison matrix.
    ParetoStatus,
    /// A gap the gap rules found.
    Gap,
    /// A blindspot audit finding.
    Finding,
    /// A ranked next step.
    NextStep,
    /// The reason of an advisory measurement flag (its ID is still shown).
    FlagReason,
    /// A reviewed derivative a review copy would attach.
    Attachment,
    /// An artifact entry of a producer-bundle run.
    ArtifactEntry,
    /// The retained bytes of an artifact a producer bundle lists.
    ArtifactBytes,
    /// A captured run's finalized manifest, verbatim.
    CaptureManifest,
    /// A run's envelope to re-import.
    Envelope,
    /// The captured bytes of a human file a record relies on.
    HumanFileBytes,
}

impl Omitted {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lesson => "lesson",
            Self::Step => "step",
            Self::MatrixRow => "matrix_row",
            Self::MatrixCell => "matrix_cell",
            Self::ParetoStatus => "pareto_status",
            Self::Gap => "gap",
            Self::Finding => "finding",
            Self::NextStep => "next_step",
            Self::FlagReason => "flag_reason",
            Self::Attachment => "attachment",
            Self::ArtifactEntry => "artifact_entry",
            Self::ArtifactBytes => "artifact_bytes",
            Self::CaptureManifest => "capture_manifest",
            Self::Envelope => "envelope",
            Self::HumanFileBytes => "human_file_bytes",
        }
    }
}

/// Why a kind left content out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    /// It cites, reads or was computed from a record the audience policy
    /// withheld.
    RestsOnWithheld,
    /// It cites a record the case snapshot does not hold, so what it rests
    /// on cannot be judged for the audience.
    RestsOnUnresolved,
    /// Its bytes hold record identifiers or a journal head, which an export
    /// may rewrite; a rewritten copy would not be the recorded bytes.
    BytesHoldIdentifiers,
    /// This export remaps identifiers, which would rewrite these bytes.
    IdentifiersRemapped,
    /// Its retained bytes were not read for this export.
    BytesNotRead,
}

impl OmissionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RestsOnWithheld => "rests_on_withheld",
            Self::RestsOnUnresolved => "rests_on_unresolved",
            Self::BytesHoldIdentifiers => "bytes_hold_identifiers",
            Self::IdentifiersRemapped => "identifiers_remapped",
            Self::BytesNotRead => "bytes_not_read",
        }
    }
}

/// One piece of content a kind left out. For the publisher only, like an
/// [`super::Exclusion`]: it is never written into the export.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Omission {
    pub content: Omitted,
    pub reason: OmissionReason,
    /// The record the content belongs to or is about, when there is one
    /// (canonical, as in the case).
    pub record: Option<Id>,
    /// Which part of it, when the record has several: a field and index
    /// (`lessons[2]`), a gap ID, a criterion ID or an item name.
    pub part: Option<String>,
}

impl Omission {
    pub const fn new(content: Omitted, reason: OmissionReason) -> Self {
        Self {
            content,
            reason,
            record: None,
            part: None,
        }
    }

    /// The record the content belongs to.
    #[must_use]
    pub const fn of(mut self, record: Id) -> Self {
        self.record = Some(record);
        self
    }

    /// Which part of the record.
    #[must_use]
    pub fn at(mut self, part: impl Into<String>) -> Self {
        self.part = Some(part.into());
        self
    }
}

/// The omissions one plan reports, in a fixed order whatever order the
/// kind reports them in; reporting the same omission twice (a kind that
/// renders the same content in two files) keeps one.
#[derive(Debug, Default)]
pub struct Omissions(RefCell<BTreeSet<Omission>>);

impl Omissions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&self, omission: Omission) {
        self.0.borrow_mut().insert(omission);
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// Every omission reported so far, in order.
    pub fn list(&self) -> Vec<Omission> {
        self.0.borrow().iter().cloned().collect()
    }
}

/// What made an export incomplete, as far as its audience is told. See the
/// module documentation for the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortfall {
    /// Nothing was withheld or left out.
    None,
    /// The audience policy withheld records; content may be left out too.
    Withheld,
    /// Nothing was withheld, but the kind left content out.
    LeftOut,
}

impl Shortfall {
    /// The shortfall of an export with these exclusions and omissions.
    pub const fn of(withheld: bool, omitted: bool) -> Self {
        match (withheld, omitted) {
            (true, _) => Self::Withheld,
            (false, true) => Self::LeftOut,
            (false, false) => Self::None,
        }
    }

    pub const fn completeness(self) -> Completeness {
        match self {
            Self::None => Completeness::Complete,
            Self::Withheld | Self::LeftOut => Completeness::Incomplete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_omission_or_exclusion_reads_incomplete() {
        assert_eq!(
            Shortfall::of(false, false).completeness(),
            Completeness::Complete
        );
        for (withheld, omitted) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                Shortfall::of(withheld, omitted).completeness(),
                Completeness::Incomplete
            );
        }
        // Withholding decides the statement, so an omission beside it is
        // never told apart.
        assert_eq!(Shortfall::of(true, true), Shortfall::of(true, false));
    }

    #[test]
    fn omissions_are_ordered_and_deduplicated() {
        let omissions = Omissions::new();
        let gap = Omission::new(Omitted::Gap, OmissionReason::RestsOnWithheld).at("g2");
        omissions.report(gap.clone());
        omissions.report(Omission::new(Omitted::Gap, OmissionReason::RestsOnWithheld).at("g1"));
        omissions.report(gap);
        let parts: Vec<_> = omissions
            .list()
            .into_iter()
            .map(|omission| omission.part.unwrap())
            .collect();
        assert_eq!(parts, ["g1", "g2"]);
    }

    #[test]
    fn words_match_their_serialized_form() {
        for content in [Omitted::Lesson, Omitted::MatrixRow, Omitted::HumanFileBytes] {
            assert_eq!(
                serde_json::to_value(content).unwrap(),
                serde_json::json!(content.as_str())
            );
        }
        for reason in [
            OmissionReason::RestsOnWithheld,
            OmissionReason::RestsOnUnresolved,
            OmissionReason::BytesHoldIdentifiers,
            OmissionReason::IdentifiersRemapped,
            OmissionReason::BytesNotRead,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::json!(reason.as_str())
            );
        }
    }
}
