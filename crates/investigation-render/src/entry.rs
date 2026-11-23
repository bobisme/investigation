//! Projection health on the entry note during a working-vault render.
//!
//! The plan renders `Start Here.md` healthy, so a plan stays a function of
//! the snapshot and the policy. A working-vault render knows more: the drift
//! the manifest records, and whether the render itself stopped early. It
//! publishes the entry note that says so ([`crate::start_here::EntryHealth`]):
//!
//! - Before publishing, the plan's entry note is re-planned with the drift
//!   recorded by the last render, so a vault whose drift has not changed is
//!   written once, with the note that announces it.
//! - After publishing, when the drift this render diagnosed differs, or the
//!   render stopped on an error, the entry note is written again with what
//!   is now known. On the error paths this is best effort: a partial render
//!   is announced on the page whenever the page itself can still be written.
//!
//! The extra write follows the same rules as every other one: the manifest
//! must accept what is at the path (a drifted or foreign entry note is left
//! as found), the new hash is recorded as a write-ahead intent before the
//! file changes, and the path is owned at the new hash afterwards. A render
//! killed during that write therefore leaves `updating`, which the next
//! render completes.

use investigation_store::Workspace;

use crate::{
    Action, Drift, Manifest, Outcome, PlanEntry, Publisher, RenderError, RenderLock, RenderPlan,
    drift::{Guard, Survey},
    layout,
    start_here::EntryHealth,
};

/// The health to announce: `partial` with its reason, and `drift` as
/// recorded.
pub(crate) fn health<'a>(
    partial: Option<String>,
    drift: impl Iterator<Item = &'a Drift>,
) -> EntryHealth {
    EntryHealth {
        partial,
        drift: drift
            .map(|drift| (drift.path.clone(), drift.kind))
            .collect(),
    }
}

/// The entry note of `announced`, when it differs from what `manifest`
/// owns at that path.
fn changed<'p>(announced: &'p RenderPlan, manifest: &Manifest) -> Option<&'p PlanEntry> {
    let path = layout::start_here().ok()?;
    let entry = announced.entry(path.as_str())?;
    let owned = manifest
        .get(&path)
        .is_some_and(|record| &record.sha256 == entry.sha256());
    (!owned).then_some(entry)
}

/// Write the entry note of `announced` over the one `manifest` records, and
/// return the manifest with the outcome. `manifest` is the record the render
/// is about to store; it is not stored here except as the write-ahead
/// intent, so the caller stores the result.
pub(crate) fn publish(
    workspace: &Workspace,
    lock: &RenderLock,
    publisher: &Publisher<'_>,
    survey: &Survey,
    manifest: Manifest,
    announced: &RenderPlan,
) -> Result<(Manifest, Option<Outcome>), RenderError> {
    let Some(entry) = changed(announced, &manifest) else {
        return Ok((manifest, None));
    };
    let generation = announced.generation();
    let intent = manifest.intending(entry, generation);
    intent.store(workspace, lock)?;
    let ownership = Guard {
        manifest: &intent,
        survey,
    };
    let outcome = publisher.apply(generation, entry, &ownership)?;
    let manifest = match outcome.action {
        Action::Written | Action::Unchanged => manifest.owning(entry, generation),
        Action::Removed | Action::Refused(_) => manifest,
    };
    Ok((manifest, Some(outcome)))
}
