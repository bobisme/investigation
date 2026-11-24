//! Disposable checkpoints of verified case heads (PERF.01, §10.5–§10.6).
//!
//! Every commit and read verifies the cases it loads under the commit lock
//! (ADR 0011 limits a command to the cases it reads). A full
//! verification parses, bounds and validates every event; on a journal of
//! tens of megabytes that dominated the cost of a one-operation commit. Once
//! a commit publishes, the heads its load verified are kept under
//! `cache/journal-heads/`; reads, retries and refused commands use kept
//! heads but never write one. The
//! next load hands it to [`replay_case_from`], which still reads and hashes
//! every event and still runs every check that relates events to one another
//! and to the state, but skips the per-event checks that are a pure function
//! of bytes already verified, and only if the chain ends at exactly that
//! head. A chain detects inconsistencies relative to a known head (§10.6): the
//! checkpoint is that known head, and events after it are verified in full.
//!
//! The checkpoint is never canonical. An absent, unreadable, unparsable or
//! foreign entry, a journal that differs from it or ends before it, all mean
//! a full verification, whose result is returned. A shorter journal is not
//! an error by itself: a crash can lose an event that a reader already saw
//! before its directory sync. An entry is bound to the workspace, the case
//! and the program that verified the head ([`verifier`]): another build may
//! verify differently, so its entries are ignored. `check` never reads one.
//! It is not a defence against a writer of the workspace, who can change the
//! canonical files as easily; see ADR 0010.

use std::sync::OnceLock;

use investigation_core::wire::Digest;

use super::*;
use crate::journal::KnownHead;

/// Where checkpoints live, one file per case.
const DIRECTORY: &str = "cache/journal-heads";
/// The first word of an entry. A different word is a miss.
const FORMAT: &str = "inv.journal-head.v1";

/// The program that verifies: its version and the identity of its executable
/// file, so a rebuilt or reinstalled program verifies every head once again.
/// `None` when the executable cannot be identified; checkpoints are then
/// neither read nor written.
fn verifier() -> Option<&'static str> {
    static VERIFIER: OnceLock<Option<String>> = OnceLock::new();
    VERIFIER
        .get_or_init(|| {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
            Some(format!(
                "{}+{}.{}.{}.{}.{}.{}.{}",
                env!("CARGO_PKG_VERSION"),
                metadata.dev(),
                metadata.ino(),
                metadata.size(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec()
            ))
        })
        .as_deref()
}

fn entry_path(case_id: Id) -> PathBuf {
    Path::new(DIRECTORY).join(case_id.to_string())
}

fn entry(verifier: &str, workspace_id: Id, case_id: Id, snapshot: &Snapshot) -> String {
    format!(
        "{FORMAT} {verifier} {workspace_id} {case_id} {} {}\n",
        snapshot.sequence, snapshot.head_hash
    )
}

fn parse(bytes: &[u8], verifier: &str, workspace_id: Id, case_id: Id) -> Option<KnownHead> {
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
    let mut words = text.split(' ');
    let matches = words.next()? == FORMAT
        && words.next()? == verifier
        && words.next()?.parse::<Id>().ok()? == workspace_id
        && words.next()?.parse::<Id>().ok()? == case_id;
    let sequence = words
        .next()?
        .parse::<u64>()
        .ok()
        .filter(|&sequence| sequence > 0)?;
    let head_hash = words.next()?.parse::<Digest>().ok()?;
    (matches && words.next().is_none()).then_some(KnownHead {
        sequence,
        head_hash,
    })
}

/// The head of `case_id` this program verified earlier, if one is kept.
/// It reads the filesystem alone, so a replay can run apart from its store.
pub(super) fn known_head_in(
    fs: &impl DurableFs,
    workspace_id: Id,
    case_id: Id,
) -> Option<KnownHead> {
    let bytes = fs.read_cache(&entry_path(case_id))?;
    parse(&bytes, verifier()?, workspace_id, case_id)
}

impl<F: DurableFs> Store<F> {
    /// [`known_head_in`] for this store's workspace.
    #[cfg(test)]
    pub(super) fn known_head(&self, case_id: Id) -> Option<KnownHead> {
        known_head_in(&self.fs, self.workspace.id(), case_id)
    }

    /// Keep the head of `case_id` that a load just verified.
    pub(super) fn keep_head(&self, case_id: Id, snapshot: &Snapshot) {
        if let Some(verifier) = verifier() {
            self.fs.write_cache(
                &entry_path(case_id),
                entry(verifier, self.workspace.id(), case_id, snapshot).as_bytes(),
            );
        }
    }
}

#[cfg(test)]
#[path = "journal_heads_tests.rs"]
mod scenarios;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_applies_only_to_its_program_workspace_and_case() {
        let workspace: Id = "ws_01a20914-25fa-7041-91e9-0fe450b811f5".parse().unwrap();
        let case: Id = "case_01a20914-25fa-7041-91e9-0fe450b811f5".parse().unwrap();
        let other: Id = "case_01a20914-25fa-7041-91e9-0fe450b811f6".parse().unwrap();
        let snapshot = Snapshot {
            sequence: 7,
            head_hash: hash_bytes(b"head"),
        };
        let line = entry("v", workspace, case, &snapshot);
        let known = KnownHead {
            sequence: 7,
            head_hash: snapshot.head_hash.clone(),
        };
        assert_eq!(parse(line.as_bytes(), "v", workspace, case), Some(known));
        // Another program, case or format, or a damaged entry: a miss.
        assert_eq!(parse(line.as_bytes(), "w", workspace, case), None);
        assert_eq!(parse(line.as_bytes(), "v", workspace, other), None);
        assert_eq!(
            parse(
                line.replace(FORMAT, "inv.journal-head.v0").as_bytes(),
                "v",
                workspace,
                case
            ),
            None
        );
        assert_eq!(
            parse(&line.as_bytes()[..line.len() - 2], "v", workspace, case),
            None
        );
        assert_eq!(
            parse(line.replace(" 7 ", " 0 ").as_bytes(), "v", workspace, case),
            None
        );
        assert_eq!(
            parse(format!("{line}x").as_bytes(), "v", workspace, case),
            None
        );
        assert!(verifier().is_some());
    }
}
