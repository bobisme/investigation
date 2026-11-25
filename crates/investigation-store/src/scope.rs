//! Case-scoped verification (PERF.06, ADR 0011).
//!
//! A command that names one case verifies that case, and the other cases it
//! reads, before it uses them; it does not verify the rest of the workspace.
//! Each loaded case is verified as [`Store::load_verified`] verifies every
//! case: from its genesis, with a kept head (ADR 0010) and a full check of the
//! tail. `check` and the workspace-wide reads still verify every byte.
//!
//! A commit still answers two workspace-wide questions (ADR 0002): whether its
//! actor and command ID are already recorded in any case, and whether the IDs
//! it allocates are. For the loaded cases the answer comes from their verified
//! events. For every other case it comes from the disposable index, and only
//! when the index's head of that case is the case's canonical head: the event
//! file at the indexed sequence exists and the next one does not. Event files
//! are published once, in sequence, under the commit lock this load holds, so
//! a case that any writer extended since the index applied it fails that test
//! and is verified and answered from its events instead. An index that is
//! absent, of another version or workspace, names a case that is not
//! published, or cannot answer, gives way to a verified load of every case. A
//! key the index places in a case is answered from that case's verified
//! events, never from the index alone.
//!
//! An acquisition key or a producer attempt can also be recorded in any case
//! (PERF.09). The index lists the cases whose applied events claimed each
//! one, and a commit that searches for a claim verifies those cases as well,
//! so its preparation searches exactly the cases that can hold the claim.

use super::*;
use crate::journal::KnownHead;

/// What a commit's preparation reads beyond its target case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reads {
    /// The target case, and the cases the commit names: lesson sources, and
    /// the case that already holds the command.
    Target,
    /// As [`Reads::Target`], and every case that may hold one of these
    /// claims: an acquisition key or a producer attempt, which can be
    /// recorded in any case (PERF.09). The index names the cases that claim
    /// each one; a case it is not current for is verified anyway.
    Claims(Vec<Claim>),
}

/// The verified cases a commit started from, and the index that answers for
/// the cases it did not load.
pub(super) struct Scope {
    pub(super) replay: FreeLater<WorkspaceReplay>,
    /// Answers for every published case not in `loaded`; `None` once every
    /// case is loaded.
    index: Option<Projection>,
    /// The cases verified into this scope, including a target the commit
    /// has since taken out of `replay`.
    loaded: BTreeSet<Id>,
}

impl<F: DurableFs> Store<F> {
    /// Published case IDs, from the names in the cases directory, in order.
    /// Nothing is verified. An entry that is not a case ID is an integrity
    /// failure, as it is for a verified load.
    pub(super) fn published_cases(&self) -> Result<Vec<Id>, Failure> {
        let mut cases = self
            .fs
            .list(Path::new(CASES))
            .map_err(io_failure)?
            .into_iter()
            .map(|directory| {
                directory
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.parse::<Id>().ok())
                    .filter(|id| id.kind() == Kind::Case)
                    .ok_or_else(|| integrity("Canonical cases directory has an invalid entry."))
            })
            .collect::<Result<Vec<_>, _>>()?;
        cases.sort_unstable();
        Ok(cases)
    }

    /// Verify one published case into `into`, with the workspace-wide checks
    /// among the cases `into` holds. `Ok(false)` when it is not published.
    pub(super) fn load_case_into(
        &self,
        into: &mut WorkspaceReplay,
        case_id: Id,
        reuse_heads: bool,
    ) -> Result<bool, Failure> {
        let Some((replay, known)) = self.replay_published(case_id, reuse_heads)? else {
            return Ok(false);
        };
        Self::admit(into, case_id, replay, known.as_ref(), reuse_heads)?;
        Ok(true)
    }

    /// The first half of [`Store::load_case_into`]: verify one published
    /// case on its own, with the head kept for it when `reuse_heads`.
    /// `Ok(None)` when it is not published. It reads only, so cases can be
    /// replayed in parallel and then admitted in order.
    pub(super) fn replay_published(
        &self,
        case_id: Id,
        reuse_heads: bool,
    ) -> Result<Option<(CaseReplay, Option<KnownHead>)>, Failure> {
        replay_published_in(&self.fs, &self.workspace, case_id, reuse_heads)
    }

    /// The second half of [`Store::load_case_into`]: the workspace-wide
    /// checks of one verified case against the cases `into` already holds,
    /// then the case is added to `into`.
    pub(super) fn admit(
        into: &mut WorkspaceReplay,
        case_id: Id,
        replay: CaseReplay,
        known: Option<&KnownHead>,
        reuse_heads: bool,
    ) -> Result<(), Failure> {
        if reuse_heads
            && known.is_none_or(|known| {
                known.sequence != replay.snapshot.sequence
                    || known.head_hash != replay.snapshot.head_hash
            })
        {
            into.unkept.push((case_id, replay.snapshot.clone()));
        }
        for (offset, event) in replay.events.iter().enumerate() {
            if !into.transactions.insert(event.event.transaction_id) {
                return Err(integrity(
                    "A transaction ID appears twice in the workspace.",
                ));
            }
            let key = (event.event.actor.clone(), event.event.command_id.clone());
            if into.commands.insert(key, (case_id, offset)).is_some() {
                return Err(integrity(
                    "An actor and command ID appear twice in the workspace.",
                ));
            }
            for change in &event.event.accepted.created {
                if !into.records.insert(change.id) {
                    return Err(integrity("A record ID was created twice in the workspace."));
                }
            }
        }
        into.cases.insert(case_id, replay);
        Ok(())
    }

    /// Verify `case_id` alone, reusing its kept head. `None` when it is not
    /// published.
    pub(super) fn load_case_verified(&self, case_id: Id) -> Result<Option<CaseReplay>, Failure> {
        let mut replay = WorkspaceReplay::default();
        Ok(if self.load_case_into(&mut replay, case_id, true)? {
            replay.cases.remove(&case_id)
        } else {
            None
        })
    }

    /// Verify the cases a commit to `target` reads before its preparation
    /// runs. The caller holds the coordination lock.
    pub(super) fn load_scope(&self, target: Option<Id>, reads: Reads) -> Result<Scope, Failure> {
        let claims = match reads {
            Reads::Target => Vec::new(),
            Reads::Claims(claims) => claims,
        };
        let published = self.published_cases()?;
        let mut replay = WorkspaceReplay::default();
        if let Some(target) = target {
            self.load_case_into(&mut replay, target, true)?;
        }
        let Ok(index) = Projection::open_read_only(&self.index_path(), self.workspace.id()) else {
            return self.full_scope();
        };
        let Ok(heads) = index.head_files() else {
            return self.full_scope();
        };
        if heads
            .iter()
            .any(|(case_id, _, _)| published.binary_search(case_id).is_err())
        {
            return self.full_scope();
        }
        let heads: BTreeMap<Id, (Snapshot, Option<String>)> = heads
            .into_iter()
            .map(|(case_id, head, file)| (case_id, (head, file)))
            .collect();
        for case_id in &published {
            if replay.cases.contains_key(case_id)
                || heads.get(case_id).is_some_and(|(head, file)| {
                    self.is_canonical_head(*case_id, head.sequence, file.as_deref())
                })
            {
                continue;
            }
            // Extended, shortened or never applied: verify it here.
            self.load_case_into(&mut replay, *case_id, true)?;
        }
        // Every case that may hold a claim the preparation searches for. The
        // index lists each case whose applied events claimed it; every case
        // not loaded by now is current in the index, so a case it does not
        // list holds no such claim.
        for claim in &claims {
            let Ok(cases) = index.claim_cases(claim) else {
                return self.full_scope();
            };
            for case_id in cases {
                if published.binary_search(&case_id).is_err() {
                    return self.full_scope();
                }
                if !replay.cases.contains_key(&case_id) {
                    self.load_case_into(&mut replay, case_id, true)?;
                }
            }
        }
        replay.located.extend(claims);
        let loaded: BTreeSet<Id> = replay.cases.keys().copied().collect();
        if loaded.len() == published.len() {
            replay.complete = true;
        }
        Ok(Scope {
            index: (!replay.complete).then_some(index),
            replay: FreeLater::new(replay),
            loaded,
        })
    }

    fn full_scope(&self) -> Result<Scope, Failure> {
        let replay = self.load_verified()?;
        Ok(Scope {
            loaded: replay.cases.keys().copied().collect(),
            replay: FreeLater::new(replay),
            index: None,
        })
    }

    /// Whether event `sequence` is the last published event of `case_id`,
    /// and its file is the one the index applied: the same identity
    /// (device, inode, size, modification and status-change times). A file
    /// published again at that sequence, after a crash lost the one the
    /// index applied, has another identity (PERF.09).
    fn is_canonical_head(&self, case_id: Id, sequence: u64, indexed: Option<&str>) -> bool {
        let events = case_directory(case_id).join("events");
        let file = |sequence: u64| event_filename(sequence).map(|name| events.join(name));
        let (Ok(head), Ok(next)) = (file(sequence), file(sequence.saturating_add(1))) else {
            return false;
        };
        indexed.is_some_and(|indexed| {
            self.fs
                .identity(&head)
                .is_ok_and(|identity| identity.key() == indexed)
        }) && matches!(self.fs.identity(&next), Err(error) if error.kind() == io::ErrorKind::NotFound)
    }

    /// The verified event that recorded `(actor, command_id)` in any case, as
    /// its case and offset in `scope.replay`. A case the index names is
    /// verified first; an index that names a case whose verified events do
    /// not hold the key gives way to a verified load of every case.
    pub(super) fn find_command(
        &self,
        scope: &mut Scope,
        actor: &Actor,
        command_id: &str,
    ) -> Result<Option<(Id, usize)>, Failure> {
        let key = (actor.clone(), command_id.to_owned());
        if let Some(found) = scope.replay.commands.get(&key) {
            return Ok(Some(*found));
        }
        let Some(index) = &scope.index else {
            return Ok(None);
        };
        match index.command_case(actor, command_id) {
            Ok(None) => return Ok(None),
            // A loaded case is answered from its events alone.
            Ok(Some(case_id)) if scope.loaded.contains(&case_id) => return Ok(None),
            Ok(Some(case_id)) => {
                self.ensure_loaded(scope, case_id)?;
                if let Some(found) = scope.replay.commands.get(&key) {
                    return Ok(Some(*found));
                }
            }
            Err(_) => {}
        }
        *scope = self.full_scope()?;
        Ok(scope.replay.commands.get(&key).copied())
    }

    /// Verify `case_id` into the scope if it is published and not loaded.
    pub(super) fn ensure_loaded(&self, scope: &mut Scope, case_id: Id) -> Result<(), Failure> {
        if scope.loaded.contains(&case_id) || scope.index.is_none() {
            return Ok(());
        }
        if self.load_case_into(&mut scope.replay, case_id, true)? {
            scope.loaded.insert(case_id);
        }
        Ok(())
    }

    /// Every verified record whose ID starts with the typed `prefix` (a full
    /// ID is its own prefix), by the case that holds it (bn-1lau). Cases the
    /// index is not current for are verified and searched; for the others the
    /// index names the cases that may hold a match, and those are verified
    /// and searched. An index that cannot answer gives way to a verified load
    /// of every case. An untyped prefix matches nothing.
    pub fn locate_records(&mut self, prefix: &str) -> Result<BTreeMap<Id, Vec<Id>>, Failure> {
        let _lock = self.coordinate_read()?;
        let Some(kind) = prefix
            .split_once('_')
            .and_then(|(tag, _)| Kind::ALL.iter().copied().find(|kind| kind.prefix() == tag))
        else {
            return Ok(BTreeMap::new());
        };
        let found = |scope: &Scope| -> BTreeMap<Id, Vec<Id>> {
            scope
                .replay
                .cases
                .iter()
                .filter_map(|(case_id, replay)| {
                    let ids: Vec<Id> = replay
                        .state
                        .records_of_kind(kind)
                        .map(|record| record.header.id)
                        .filter(|id| id.to_string().starts_with(prefix))
                        .collect();
                    (!ids.is_empty()).then_some((*case_id, ids))
                })
                .collect()
        };
        let mut scope = self.load_scope(None, Reads::Target)?;
        let pointed = scope
            .index
            .as_ref()
            .map(|index| index.id_prefix_cases(kind, prefix));
        let pointed = match pointed {
            None => return Ok(found(&scope)),
            Some(Err(_)) => return Ok(found(&self.full_scope()?)),
            Some(Ok(cases)) => cases,
        };
        for case_id in &pointed {
            self.ensure_loaded(&mut scope, *case_id)?;
        }
        let located = found(&scope);
        // A case the index names that its verified events do not confirm
        // means the index is wrong: search every verified case instead.
        if pointed.iter().any(|case_id| !located.contains_key(case_id)) {
            return Ok(found(&self.full_scope()?));
        }
        Ok(located)
    }

    /// Whether any case already records one of `ids`, a record or a
    /// transaction ID. Loaded cases answer from their events, the others from
    /// the index; if the index cannot answer, every case is verified.
    pub(super) fn already_recorded(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = Id> + Clone,
    ) -> Result<bool, Failure> {
        let in_loaded = |replay: &WorkspaceReplay, id: Id| {
            replay.records.contains(&id) || replay.transactions.contains(&id)
        };
        if ids
            .clone()
            .into_iter()
            .any(|id| in_loaded(&scope.replay, id))
        {
            return Ok(true);
        }
        let Some(index) = &scope.index else {
            return Ok(false);
        };
        let mut found = false;
        for id in ids.clone() {
            match index.id_cases(id) {
                Ok(cases) => {
                    found |= cases.iter().any(|case_id| !scope.loaded.contains(case_id));
                }
                Err(_) => {
                    let full = FreeLater::new(self.load_verified()?);
                    return Ok(ids.into_iter().any(|id| in_loaded(&full, id)));
                }
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;

/// [`Store::replay_published`] from the filesystem and workspace alone, so
/// cases can be replayed on several threads without sharing the store.
pub(super) fn replay_published_in<F: DurableFs>(
    fs: &F,
    workspace: &Workspace,
    case_id: Id,
    reuse_heads: bool,
) -> Result<Option<(CaseReplay, Option<KnownHead>)>, Failure> {
    let directory = case_directory(case_id);
    if case_id.kind() != Kind::Case {
        return Ok(None);
    }
    match fs.list(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_failure(error)),
    }
    let known = reuse_heads
        .then(|| super::journal_heads::known_head_in(fs, workspace.id(), case_id))
        .flatten();
    let replay = replay_case_from(
        fs,
        &directory,
        workspace.writers(case_id),
        case_id,
        known.as_ref(),
    )?;
    if replay.events.is_empty() {
        return Err(integrity("A published case has no first transaction."));
    }
    Ok(Some((replay, known)))
}
