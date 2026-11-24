//! `Attachments/`: Investigation writes only approved derivative previews.
//!
//! Stored originals and exact source lineage stay outside the vault. A file
//! is written under `Attachments/` only when every one of these holds:
//!
//! - the path is `Attachments/<artifact-id>.<ext>` ([`layout::attachment`]);
//! - the view policy opts in ([`ViewPolicy::derivative_previews`], off by
//!   default, so a working vault holds no attachments unless asked);
//! - the artifact is in the snapshot, is a derivative (never an original or a
//!   manifest), records a review of its derivation, and is not withdrawn;
//! - the planned bytes are exactly the derivative's recorded digest, so no
//!   other bytes, an original's included, can travel under its name;
//! - the artifact's effective classification lets the policy's audience see
//!   its content (derivation inputs and reviewed downgrades included).
//!
//! The publisher asks [`crate::Ownership::approves_attachment`] before it
//! inspects any attachment entry, and the default answer is no; [`Gate`]
//! answers from [`approved`] for a working-vault render. A planner that adds
//! previews should plan only what [`approved`] accepts; the gate checks again.
//! Once written, an attachment is owned like any generated file: it is
//! replaced or removed only with proof of the bytes last generated. A file
//! the manifest does not own stays where it is and is reported as unknown.

use investigation_core::{
    classification::{Classifier, Field},
    derivation,
    evidence::{ArtifactBody, ArtifactRole},
    identity::Kind,
    source_ops::EvidentiaryStatus,
};

use crate::{
    Decision, OnDisk, Ownership, PlanEntry, PlanPath, RenderPlan, VerifiedSnapshot, ViewPolicy,
    layout,
};

/// Whether `entry` is an approved derivative preview for `policy`.
pub fn approved(snapshot: &VerifiedSnapshot, policy: &ViewPolicy, entry: &PlanEntry) -> bool {
    if !policy.derivative_previews {
        return false;
    }
    let Some(id) = layout::attachment_artifact(entry.path()) else {
        return false;
    };
    snapshot.cases().any(|case| {
        let state = case.state();
        let Some(record) = state.current(id) else {
            return false;
        };
        if record.header.id.kind() != Kind::Artifact {
            return false;
        }
        let Some(body): Option<ArtifactBody> = derivation::artifact_body(state, record.reference())
        else {
            return false;
        };
        body.role() == ArtifactRole::Derivative
            && body
                .derivation
                .as_ref()
                .is_some_and(|record| record.review.is_some())
            && body.status != Some(EvidentiaryStatus::Withdrawn)
            && body.digest.as_ref() == Some(entry.sha256())
            && Classifier::new(state).may_show(record, Field::Content, &policy.audience_policy())
    })
}

/// The working vault's ownership with attachment approval: every decision is
/// `inner`'s, and an attachment entry is approved only by [`approved`].
pub(crate) struct Gate<'a> {
    pub inner: &'a dyn Ownership,
    pub snapshot: &'a VerifiedSnapshot,
    pub policy: &'a ViewPolicy,
}

impl Ownership for Gate<'_> {
    fn decide(&self, entry: &PlanEntry, on_disk: &OnDisk) -> Decision {
        self.inner.decide(entry, on_disk)
    }

    fn retired(&self, plan: &RenderPlan) -> Vec<PlanPath> {
        self.inner.retired(plan)
    }

    fn recorded(
        &self,
        path: &PlanPath,
    ) -> Option<(
        &investigation_core::wire::Digest,
        Option<crate::fingerprint::Fingerprint>,
    )> {
        self.inner.recorded(path)
    }

    fn decide_removal(&self, path: &PlanPath, on_disk: &OnDisk) -> Decision {
        self.inner.decide_removal(path, on_disk)
    }

    fn approves_attachment(&self, entry: &PlanEntry) -> bool {
        approved(self.snapshot, self.policy, entry)
    }
}
