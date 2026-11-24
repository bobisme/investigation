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

use super::*;

/// What a commit's preparation reads beyond its target case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reads {
    /// The target case, and the cases the commit names: lesson sources, and
    /// the case that already holds the command.
    Target,
    /// Every case: the preparation searches the workspace, for an acquisition
    /// key or a producer attempt.
    Workspace,
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
        let directory = case_directory(case_id);
        if case_id.kind() != Kind::Case {
            return Ok(false);
        }
        match self.fs.list(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_failure(error)),
        }
        let known = reuse_heads.then(|| self.known_head(case_id)).flatten();
        let replay = replay_case_from(
            &self.fs,
            &directory,
            self.workspace.writers(case_id),
            case_id,
            known.as_ref(),
        )?;
        if replay.events.is_empty() {
            return Err(integrity("A published case has no first transaction."));
        }
        if reuse_heads
            && known.as_ref().is_none_or(|known| {
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
        Ok(true)
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
        if reads == Reads::Workspace {
            return self.full_scope();
        }
        let published = self.published_cases()?;
        let mut replay = WorkspaceReplay::default();
        if let Some(target) = target {
            self.load_case_into(&mut replay, target, true)?;
        }
        let Ok(index) = Projection::open_read_only(&self.index_path(), self.workspace.id()) else {
            return self.full_scope();
        };
        let Ok(heads) = index.heads() else {
            return self.full_scope();
        };
        if heads
            .iter()
            .any(|(case_id, _)| published.binary_search(case_id).is_err())
        {
            return self.full_scope();
        }
        let heads: BTreeMap<Id, Snapshot> = heads.into_iter().collect();
        for case_id in &published {
            if replay.cases.contains_key(case_id)
                || heads
                    .get(case_id)
                    .is_some_and(|head| self.is_canonical_head(*case_id, head.sequence))
            {
                continue;
            }
            // Extended, shortened or never applied: verify it here.
            self.load_case_into(&mut replay, *case_id, true)?;
        }
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

    /// Whether event `sequence` is the last published event of `case_id`.
    fn is_canonical_head(&self, case_id: Id, sequence: u64) -> bool {
        let events = case_directory(case_id).join("events");
        let file = |sequence: u64| event_filename(sequence).map(|name| events.join(name));
        let (Ok(head), Ok(next)) = (file(sequence), file(sequence.saturating_add(1))) else {
            return false;
        };
        self.fs.identity(&head).is_ok()
            && matches!(self.fs.identity(&next), Err(error) if error.kind() == io::ErrorKind::NotFound)
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
