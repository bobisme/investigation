<!-- inv.exposure-guidance.v3 -->
# A secret reached the workspace: diagnosis and manual remediation

Read this when a secret (a password, token, key or personal data) reached a workspace: in event text such as an observation statement or quote, in a retained file, or in a vault note.

What this build can do and what it cannot:

- It can list every local place that holds the text: `investigation exposure ID --format json`.
- It cannot remove the text. There is no purge command. The journal is append-only and hash-chained. A withdrawal (`artifact.withdraw`, `source.withdraw`), a retraction (`observation.retract`) or a classification change (`classification.raise`) adds an event. That event changes how the record is used and shown. The earlier bytes stay in the journal, in the retained blobs and in every copy made before.
- It can remove a retained file from canonical storage in one case only: every Artifact that records its digest, in any case, has its availability withdrawn with `artifact.expire`, and no archive pin or capture staging holds it. Then `investigation retention reclaim ARTIFACT --format json` unlinks the blob. Records, digests, quoted text and the journal stay, and so do backups and copies made before.
- It cannot recall copies outside the workspace. Distributed copies cannot be recalled automatically.

Never call a withdrawal, retraction, redaction or classification change an erasure, and never report the text as gone because one of them was recorded.

## 1. Rotate the secret first

Revoke or rotate the secret where it was issued, before anything else. Treat it as disclosed to everyone who could read the workspace, its backups, synced vault copies and exports. Nothing below makes the old value safe again.

## 2. Contain

1. Raise the classification of the exposed record, and of the Artifacts that hold the bytes, to `restricted`. Get the payload with `investigation schema classification.raise --format json` and submit it with `investigation record --input raise.json --format json`. A derivative is at least as strict as its inputs, and a raised case raises every record in it.
2. Render the vault again: `investigation vault render --format json`. Generated notes of restricted records become stubs. Copies that viewers, sync clients or backups made earlier do not change.
3. Stop syncing or sharing the workspace and its vault until the remediation is complete.

Classification is handling policy for this tool's outputs, not access control: anyone who can read the directory can read the journal.

## 3. Diagnose

- `investigation exposure ID --format json` traces a record, case, transaction or command ID: the events that hold each revision, the retained bytes of a Source or Artifact and of everything derived from it, the index, blob verdicts and the record's vault notes with their conflict copies, the generated versions a render withheld from them and the edits `vault reconcile` set aside from them.
- `investigation exposure --text-file ./exposed.txt --format json` reads every file in the workspace and lists each one that holds the text. Put the secret alone in a file, or pipe it with `--text-file -`; never pass it as an argument. One trailing line break is ignored. The text is also searched in the escaped forms the tool writes (JSON, Markdown, titles, YAML). Remove the file afterwards.
- Give both for the fullest list. The command writes nothing and never prints the text.
- Retained bytes of restricted Artifacts are listed as not opened. Add `--include-restricted` to search them: `investigation exposure --text-file - --include-restricted --format json`.

Each location has an `evidence` value:

| Evidence | Meaning |
|---|---|
| `found` | The file's bytes, or its path, hold the text in one of the searched forms. |
| `holds` | The record's own data is stored there: an event that accepted it, its retained bytes, its index rows. |
| `may_hold` | Derived from a location that holds the text, such as a note projecting the record; not confirmed by bytes. |
| `digest_only` | A blob verdict: it names the digest of bytes that hold the text, not the text. |

`not_opened` lists files that were not searched and why. `outside` lists places beyond the workspace that are never searched, starting with the configured import roots, whose original files may hold the text.

What each area is and what removing the text from it takes:

| Area | Canonical | Removing the text |
|---|---|---|
| `journal_event`, `case_genesis` | yes | Only by restore (4B) or fork (4C). Editing or deleting the file fails `investigation check`. |
| `blob` | yes | By restore or fork, or with `investigation retention reclaim ARTIFACT --format json` once every Artifact recording the digest, in any case, is expired with `artifact.expire` and nothing else holds it; `investigation retention preview ARTIFACT --format json` lists the holders first. The file is shared by every Artifact with the same digest, in any case. |
| `staging` | recovery material | Treat it as canonical: finalizing the capture commits staged items to the journal, and `investigation repair staging --format json` retires staging only after that. |
| `index` | no | Delete `cache/index.sqlite` and its `-wal` and `-shm` files, then run `investigation repair index --format json`. The rebuild puts back any text the journal still holds. |
| `blob_verdict` | no | Delete the entry; the next read hashes the blob again. |
| `vault_note` | no | Raise the classification and render again (step 2), or render after a restore or fork. |
| `render_conflict` | no | A generated version a render withheld from a drifted note, under `local/conflicts/<generation>/`. It stays after the drift is reconciled. Once it is, `investigation vault clean-conflicts --area withheld --apply --format json` removes it while its bytes still hash to what the render manifest recorded; run it without `--apply` first to see the list. Otherwise remove it by hand. |
| `reconcile_set_aside` | no | Observed bytes of a drifted note, such as an edit, that `vault reconcile` set aside on discard or propose-change, under `local/conflicts/discarded/` or `local/conflicts/proposed/`. `investigation vault clean-conflicts --apply --format json` removes them while their bytes still hash to the name of their folder; run it without `--apply` first to see the list. Otherwise remove them by hand. |
| `vault_conflict_copy`, `vault`, `export`, `temporary` | no | Remove or edit the file by hand. A render never removes these. |

## 4. Choose a remediation

### 4A. The text is only in disposable or human-owned files

When no `journal_event`, `case_genesis`, `blob` or `staging` location holds it (for example, it was pasted into a human note in `Notes/`, or only an export or a conflict copy holds it): edit or remove those files by hand, remove the conflicts-area files with `investigation vault clean-conflicts --apply --format json` (the table says when it can), delete and rebuild the index as in the table, render the vault, and run the diagnosis again until it lists nothing. The journal is untouched, so no historical guarantee changes.

### 4B. Restore a pre-exposure copy and replay

This needs a copy of the whole workspace directory taken before the first event that holds the text, such as a filesystem backup. This build writes no archive of its own.

1. Restore the copy into a new, empty directory. Verify it: `investigation --workspace ./restored check --format json`.
2. Re-enter the work recorded after the copy was taken, without the secret: submit each later command's input with the secret removed under a new command ID (`investigation --workspace ./restored record --input sanitized.json --format json`), and ingest sanitized files in place of exposed ones.
3. Confirm with `investigation --workspace ./restored exposure --text-file ./exposed.txt --format json`: it must list no location.
4. Retire the exposed workspace (step 5). Never let both copies accept writes: they share one workspace identity, and two writers under one identity are not one coordinated history.

### 4C. Rebuild a sanitized workspace as a deliberate fork

Use this when no clean copy exists.

1. Create a new workspace: `investigation init ./sanitized --import-root ./clean-inputs`.
2. Re-create each case and record its work again from sanitized inputs: redacted files, rewritten statements and quotes. Every record gets a new ID.
3. Record the mapping in each new case, for example as a note on the case (`investigation note --on CASE_ID 'Mapping from the exposed workspace: ...'`): the old workspace ID, each old case's sequence and head hash, and old record ID to new record ID. The mapping never contains the secret. It is an attributed claim; `check` cannot verify it.
4. Confirm with `investigation --workspace ./sanitized exposure --text-file ./exposed.txt --format json`, then retire the exposed workspace (step 5).

### Which historical replay guarantees change (4B and 4C)

- Re-entered events have new sequence numbers, recording times and head hashes. Snapshots (sequence and head hash) quoted in earlier briefs, receipts, review packets, vault notes and exports no longer verify against the new workspace.
- Historical reads (`investigation show ID --at 3`, snapshot-pinned reads) cover only the new history. The exposed history cannot be replayed from the new workspace.
- `investigation receipt KEY` finds only commands re-entered under that key. Retrying an old command ID records anew.
- Sanitized files have new digests. Observations and selectors that cited the old bytes must be recorded again against the new Artifacts, and quotes that held the secret change.
- In a fork every record ID and the workspace identity change, so references to old IDs from other workspaces, lessons or documents dangle. In a restore, records before the restore point keep their IDs; records re-entered after it get new ones.
- The new hash chain proves only that the new history is internally consistent. Nothing in it proves that it reproduces the old history; that rests on the mapping and on who recorded it.

## 5. Retire the exposed copy

- Removing files with ordinary tools unlinks them. It does not guarantee that the bytes are gone: journaling and copy-on-write filesystems, snapshots, device remapping and backups keep copies. Do not report the old copy as erased.
- Protection at that level comes from storage, not from this tool: OS disk encryption, or an established external encrypted archive for backups. This project provides no cryptosystem of its own.
- Find the copies outside the workspace: backups, synced or shared vault folders, exports sent to others, other clones, the original files under the import roots, terminal scrollback, shell history and logs. Ask their holders to remove them, and rely on the rotation from step 1.

## 6. Do not

- Do not edit or delete journal events or blobs in place. `investigation check --format json` then reports an integrity failure and commands that replay the case refuse to run, while backups still hold the old bytes.
- Do not paste the secret into a command line, a note, an issue or a report while you diagnose it.

## Not in this build

A purge workflow: a new storage epoch built from a sanitized journal, with a redaction audit and mapping that omit the secret, and a statement of which replay guarantees changed. It is deferred until that epoch, audit and archive behavior is specified and tested; the deferred register in the repository (`docs/deferred.md`) records it.
