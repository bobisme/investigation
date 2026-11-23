# ADR 0010 – Verified-head checkpoints for journal verification

Accepted with PERF.01 (bn-30dc); 2026-11-23. Refines plan §10.5 and §10.6. [Module](../../crates/investigation-store/src/journal_heads.rs) · [Reader](../../crates/investigation-store/src/journal.rs)

**Problem.** Every commit, and every read through the store, verifies the whole journal under the commit lock before it validates anything. On the `just bench` large fixture (10,101 objects, 418 events, 22 MB) a one-operation `record` took 408 ms p50 and 428 ms p95 against a 150 ms target. A profile split it as follows: the verified replay took about 205 ms. Of that, the bounded duplicate-key parse took 70 ms, the path-tracked decode from a `Value` took 105 ms, and the metadata validation and after-image application took the rest. Two deep clones of the state took about 56 ms: the commit's projected state and the validation scratch state. Freeing the replay under the lock took about 50 ms.

**Decision: keep a verified head, and verify the bytes against it.** After a commit publishes, the store keeps each case head that the commit's load verified in `cache/journal-heads/<case-id>`. The entry is one line: format, verifier, workspace, case, sequence and head hash. The storage format does not change. The entry is derived, disposable data under `cache/`, written with the existing best-effort cache writer. The next load passes the head to `replay_case_from`. For every event, that function still does these things:

- It reads the bytes and hashes them.
- It checks the parent hash, the workspace and case identity, and duplicate transaction and command IDs.
- It checks the preconditions, applies the after-images to the state, and notes the verified snapshot.

For events at or before the kept head, the function skips only the checks that are a pure function of one event's bytes: the bounded duplicate-key parse and `validate_metadata`. Those events are decoded with a direct typed decode. That decode gives the same `Event` for any bytes the full decoder accepts (test `a_decode_of_verified_bytes_equals_the_full_decode`). The prefix is accepted only if its chain ends at exactly the kept head hash. Each event names its parent's hash, and event 1 names the genesis hash. So equal head hashes mean that every byte of the prefix and its genesis equals bytes that this program verified in full. This is §10.6 as written: a chain detects inconsistencies relative to a known head, and the checkpoint is that known head. Events after the kept head are verified in full.

**Every miss falls back to a full verification, and that result is returned.** A miss is any of these cases:

- The entry is absent, unreadable or foreign.
- A decode fails.
- A link breaks.
- The hash at the kept sequence differs.
- The journal ends before the kept head.

On a miss, the error that the caller and `check` see is the one a full replay reports. A journal shorter than the kept head is not an error by itself. A reader can see an event that a crash then loses before its directory sync (§10.4). So the shorter journal is verified in full and its head is kept. Only a commit that publishes its event writes entries: the heads its load verified, before its own event, which the next load verifies in full. Reads, idempotent retries and refused commands use entries but write none, so a read-only session such as `doctor` writes nothing. `check` never reads an entry: it always verifies every byte in full.

**The verifier binds an entry to the program.** The verifier is the package version plus the identity of the running executable: device, inode, size, and modification and change times. A rebuilt or reinstalled program can validate differently, so it ignores every entry it did not write and verifies each head in full once. No version constant has to be bumped when validation rules change.

**Further changes on the same path.**

- **Decoding.** Each window of events (up to 64 MB) is read in order and decoded on up to 8 threads. Each thread takes the next undecoded event. Each thread also copies the event's after-images for the state (`SharedImages`). Results are consumed in sequence order, so errors are reported in the same order as a sequential replay reports them.
- **State.** `State` keeps accepted revisions as `Arc<Record>`. They are immutable, so a clone shares them, and the commit and validation clones now cost about 1 ms. `State::apply_shared` takes images copied by `SharedImages::of` from the same transaction.
- **Freeing.** The commit frees its replay on a background thread (`FreeLater`), so the caller and the commit lock do not wait on deallocation.
- **Index.** The index refresh builds receipts only for the events the index has not applied.

**Integrity tests, unchanged and passing:** `real_crash_tests`, the journal tamper tests (`journal_qualification`, `production`), conformance lost-response, `closure_drill_tests` and `evidence_read_lock_tests`. New tests in `journal_heads_tests.rs` cover these cases, each with a warm head:

- A truncated head, a head with another parent, an edited request, an edited event before the head, a missing event, an unsupported version, and an edited head under an entry ahead of the journal. Each one is refused with the full-replay diagnostic, and `check` and the reads report the same diagnostic.
- Garbage, another hash, a head ahead of the journal, another workspace, and a journal shorter than the head. Each one falls back and gives the full replay's result.
- Idempotent retries and key reuse.

Two mutations were checked by hand: removing the head-hash comparison and removing the ends-before-head check. Each one makes the tamper test fail.

**Rejected alternatives.**

- *Serialize the verified state into the cache.* This needs a binary self-describing format for `serde_json::Value` bodies, which is a new dependency. It also adds a second encoding of every record, which can drift from the journal. Parsing it back would still cost about as much as the direct decode.
- *Trust file identity (inode, size, times) instead of hashing.* Hashing 22 MB costs about 9 ms, spread over the decode threads, and it proves byte equality. File identity does not prove that the bytes are equal.
- *Raise an error when the journal is shorter than the kept head.* This turns a legitimate crash outcome into a hard failure that comes from a disposable cache.

**Consequences.**

- *Trust in the cache.* For a prefix that ends at a kept head, a commit now trusts the entry's claim that those exact bytes passed `validate_metadata` in this build. A writer who can edit `cache/` can make the store accept edited events. That writer could equally write a valid chain into `state/` (§10.6). `check` still reports the edit: test `check_verifies_every_byte_even_when_a_kept_head_matches`.
- *Cost.* A commit still reads and hashes the whole journal, so its cost still grows linearly with the journal, at hashing and decode speed instead of validation speed.
- *Threads.* The store now starts up to 7 decode threads for each load, and one thread to free memory for each commit.
- *Measured.* On the large fixture with `just bench --skip-render`, on a shared 64-thread machine, `record_single_operation` went from 407.9 ms p50 and 428.3 ms p95 (1-minute load average 22.8 during the measurement) to 86.7 ms p50 and 96.1 ms p95 (load 33.5). On the sparse choice fixture it went from 135.3/161.9 ms to 27.7/31.0 ms. Full verification, which `check` and a cold cache use, also uses the parallel decode. It went from about 205 ms to about 100 ms.
