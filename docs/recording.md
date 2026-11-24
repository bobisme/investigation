# Recording API and producer adapter protocol

A producer is an external program that runs something under its own authority and records what happened. It links the `investigation-recording` crate (Rust) or sends JSON to the `investigation` CLI (any language). Both paths use one implementation and one payload shape, protocol `inv.capture.v1`. Investigation never imports, names or invokes a producer.

## Operations

| Operation | Rust call | Does | Does not |
|---|---|---|---|
| `capture.begin` | `Recorder::begin(&BeginCapture)` → `BeginReceipt` | Declares one Run (and, if asked, a minimal `record`-mode case) in one transaction, allocates private staging, returns the handle, IDs, protocol, limits and staging path. | Authorize, approve or start the producer's action; verify the producer's identity or permission. |
| `capture.append` | `Recorder::append(&CaptureEvent)` → `ItemReceipt` | Accepts one bounded lifecycle or collection fact at the next position, durably, with distinct reported and accepted times. | Start, cancel or rerun anything; reorder items or fill gaps. |
| `capture.attach` | `Recorder::attach(&StableArtifact)` → `ArtifactRef` | Copies a closed file into staging and verifies digest, size and role, or records a declared artifact as missing. | Run a parser or command; accept mutable bytes as final; hardlink producer files; replace a missing artifact with an empty one. |
| `capture.finalize` | `Recorder::finalize(&FinalizeCapture)` → `FinalizeReceipt` | Seals the handle and commits the immutable manifest: artifacts, execution state, assertions, controls, receipts, per-channel completeness. | Create an Assessment or a validated Conclusion; equate `finalized` with `execution_completed`. |
| `capture.recover` | `Recorder::recover(&RecoverScope)` → `RecoveryState` | Reads unsettled handles, or one handle: seal state, staging state, last accepted position, receipts, the original finalize receipt, and the exact commands that continue the recording. | Rerun or resume an external action; delete staged bytes; change a handle's state; infer an execution outcome. |

The crate documentation states the same limits on each method. Run `cargo doc -p investigation-recording --open` to read it.

After finalization, use the retained Artifact's ID and digest with
`investigation evidence read`. JSON output includes the exact selected binary
bytes in `excerpt.bytes_base64` for a `whole` or `byte_range` read.
[Evidence read contract](schema-v1.md#evidence-reads)

**Status.** The types and signatures are fixed. Every operation is implemented and has its CLI command: `capture begin`, `capture append`, `capture attach`, `capture finalize` and `capture recover`.

## Discovery

`investigation schema capture --format json` returns the lifecycle, the operations with their commands, and `capabilities`: the protocol versions this build accepts, the default, the operation names, the per-handle limits (event size, attachment size, positions) and the accepted `producer-run` versions. Every value comes from the code that enforces it. `investigation schema capture.begin` (and `.append`, `.attach`, `.finalize`, `.recover`) returns the request schema and an example; the `.receipt` name returns the receipt schema. The examples are the files in [fixtures/capture/v1](../fixtures/capture/v1), which the test suite runs against the CLI. Add `--schema-only` for the bare JSON Schema.

## `capture begin`

```sh
investigation --workspace ./cases capture begin --input begin.json --format json
```

`--input` takes a file, or `-` for stdin. The request is the JSON form of `BeginCapture`:

```json
{
  "key": "run-0001/begin",
  "actor": "agent:example-producer",
  "producer": {"name": "example-producer", "version": "0.1.0"},
  "external_run_id": "run-0001",
  "intent": {"label": "synthetic run"},
  "case": {"kind": "new_record_case", "title": "Synthetic runs"},
  "classification": "internal",
  "metadata": {"example-org.fixture": {"seed": 7}}
}
```

- `case` is `{"kind": "new_record_case", "title": …}` or `{"kind": "existing", "case_id": …}`. A new case has mode `record` and is created in the same transaction as the Run.
- `intent.inquiry_plan` pins an exact frozen InquiryPlan revision in an existing case. The Run then records the plan digest and its chronology. `intent.external_plan_digest` records the digest of a plan held outside Investigation.
- `protocol` is optional. Omit it to accept the version this build speaks, or name the version you require, such as `inv.capture.v1`. An unsupported version is refused with `INVALID_INPUT` at `/protocol`, with the supported list in `error.expected`, and nothing is declared.
- `classification` defaults to `internal`. `restricted` also applies to the Run record.

Stdout is the `BeginReceipt` (`handle`, `run_id`, `case_id`, `protocol`, `limits`, `staging_dir`, `commit`), the same value `Recorder::begin` returns. A rejection prints the structured `inv.response.v1` error with `ok: false`. The Run is `declared`, with evidence completeness and measurement validity `unknown`. Begin records a declaration only: it does not authorize, approve or start anything.

Idempotency:

- The key is the journal command ID. Resending the identical request returns the original receipt with `commit.result` `already_committed` and the same Run, handle and staging. Fields that take a default compare equal whether or not they are spelled out.
- The same key and actor with a different request is refused with `IDEMPOTENCY_KEY_REUSED` at `/command_id`.
- The producer name and external run ID identify one logical attempt. A second `begin` for that pair under another key or actor is refused with `IDEMPOTENCY_KEY_REUSED` at `/external_run_id`. The error names the original command ID, Run and handle, and its repair argv looks up the original receipt.
- An attempt that a producer-run envelope already recorded is refused the same way. See [Attempt identity](#attempt-identity).
- `producer.name` has 1–128 ASCII letters, digits, `.`, `_` or `-`, and starts with a letter or digit. A name with `:`, `/` or a non-ASCII character is refused at `/producer/name`.

The limits are fixed when the handle is declared: 65536 bytes per appended event, 67108864 bytes per attachment and 100000 positions. A later build with other defaults returns the original limits on a retry.

## `capture append`

```sh
investigation --workspace ./cases capture append --input event.json --format json
```

`--input` takes a file, or `-` for stdin. The request is the JSON form of `CaptureEvent`:

```json
{
  "handle": "handle_…",
  "key": "phase-summarize",
  "position": 3,
  "fact": {"kind": "phase_changed", "phase": "summarize"},
  "reported_at": {
    "timestamp": {"state": "known", "value": "2026-01-02T03:04:05.000Z"},
    "offset": {"state": "unknown"},
    "clock_quality": "unverified"
  },
  "metadata": {"example-org.host": {"slot": 3}}
}
```

| `fact.kind` | Fields | Bounds |
|---|---|---|
| `started` | — | — |
| `phase_changed` | `phase` | 1–256 characters, no control characters |
| `error` | `message`, optional `external_code` | message 1–16384 characters, kept verbatim; code 1–128 characters, no control characters |
| `cancelled` | optional `reason` | 1–4096 characters |
| `channel` | `channel`, `state` (`{"kind": "opened"}`, `{"kind": "closed"}`, or `{"kind": "gap"}` with optional `detail`) | channel 1–256 characters, no control characters; detail 1–4096 characters |

Stdout is the `ItemReceipt` (`result`, `handle`, `run_id`, `position`, `key`, `request_digest`, `accepted_at`), the same value `Recorder::append` returns. `reported_at` is the producer's claim and stays in the staged request. `accepted_at` is the recorder's local acceptance time. Neither orders the items; the position does. The serialized request may be at most `max_event_bytes` (65536). Put high-frequency samples in an attached artifact, not in individual facts.

The request carries no actor and no case. The handle names the Run, its case and the actor that declared it, and every item is attributed to that declaration.

Ordering and idempotency:

- The handle accepts only its next position, starting at 1. Any other position is refused with `REVISION_CONFLICT` at `/position`. `expected` holds the next `position` and the `last_position`; the message names the missing positions. Nothing is staged, and the gap stays fillable.
- The key is the producer's item key, unique within the handle. An identical retry returns the original receipt with `result` `already_accepted`, the original position and the original `accepted_at`. Fields that take a default compare equal whether or not they are spelled out.
- The same key with a different request, including another position, is refused with `IDEMPOTENCY_KEY_REUSED` at `/key`. `expected` holds the accepted item's `position` and `request_digest`.
- A handle that was never declared is refused with `UNKNOWN_REFERENCE` at `/handle`. A handle whose staging a crash left unallocated is refused with `OBJECT_REQUIRED`; resend the identical `begin`, then the item.
- A position above `max_items` (100000) is refused with `INVALID_INPUT` at `/position`.
- A sealed or finalized handle refuses new items with `REVISION_CONFLICT` at `/handle`, the closed-handle response. An identical retry of an accepted item still returns its receipt, also after staging is retired. See [Late items](#late-items-after-sealing).

Appending records a report. It does not start, cancel or rerun anything; a `cancelled` fact cancels nothing.

### Durability and placement

An appended fact is staged, not journaled. Each accepted item is one immutable file in the handle's staging directory. No item is its own journal transaction, so a stream of facts never becomes a stream of global commits, and the workspace commit lock is held only for a short read of the handle's case. Finalization seals staging and commits the Run's manifest in one transaction, which binds every item key, digest and receipt.

Before the receipt is returned, the item file is written, synced, published with an exclusive link, and its directory is synced. That one file holds the request bytes, the receipt and the cursor advance. Item `p` is published only after item `p - 1` is durable, so positions stay contiguous after any crash, and the last position is the highest item file. A crash before the receipt leaves the item either absent, so a retry accepts it, or present, so a retry returns its receipt. An acknowledged item survives a crash at its position.

Calls on one handle are serialized by a persistent lock file in its staging directory. That lock is separate from the workspace commit lock and is always taken first.

## `capture attach`

```sh
investigation --workspace ./cases capture attach --input artifact.json --format json
```

`--input` takes a file, or `-` for stdin. The request is the JSON form of `StableArtifact`:

```json
{
  "handle": "handle_…",
  "key": "result",
  "position": 4,
  "role": "original-output",
  "source": {"kind": "file", "path": "run-0001/result.json"},
  "channel": "results",
  "media_type": "application/json",
  "expected_digest": "sha256:…",
  "expected_size_bytes": 2048,
  "classification": "internal",
  "metadata": {"example-org.host": {"slot": 3}}
}
```

| Field | Meaning |
|---|---|
| `role` | `original-output`, `derivative`, `log` or `receipt`. A `derivative` must set `derived_from`; no other role may. |
| `source` | `{"kind": "file", "path": …}`, or `{"kind": "missing", "reason": …}` (1–4096 characters) for a declared artifact the producer could not supply. The path is at most 4096 bytes. |
| `derived_from` | `{"from": <item key>, "transform": <name/version>}`. `from` names an earlier attachment on the same handle that has retained bytes. `transform` has 1–256 characters, no control characters. |
| `channel` | Optional, 1–256 characters, no control characters. |
| `media_type` | Optional hint such as `application/json`, at most 255 printable ASCII characters. It never selects a parser. |
| `expected_digest`, `expected_size_bytes` | Optional. A mismatch refuses the item. |
| `classification` | Optional. It may be stricter than the Run's effective level, never looser. The default is the Run's level. |

Stdout is the `ArtifactRef` (`receipt`, `role`, `availability`, and `digest` and `size_bytes` when the bytes are retained), the same value `Recorder::attach` returns. `receipt` is an `ItemReceipt`. Positions, keys, replay, the sealed-handle refusal and the `max_event_bytes` bound on the serialized request work as for `append`; the two share one position sequence and one key space.

How the bytes are taken:

1. The path is resolved only beneath the workspace's `import_roots` in `investigation.toml`. A relative path is tried beneath each root in order. An absolute path must lie beneath one. No symlink is followed at any component. A path with `..`, a file that is not regular, a file with more than one hard link, or a file over `max_attachment_bytes` (64 MiB) is refused with `INVALID_INPUT` at `/source/path`. With no import root configured, every `file` source is refused.
2. The file is opened once. Its bytes are copied into the handle's staging while they are hashed. The same descriptor is then read again and hashed again. Both passes, the copied length and the descriptor's inode, size, link count, and modification and change times must agree. Otherwise the item is refused with `REVISION_CONFLICT` at `/source/path`, with `retry` set. Retry the identical request after the producer has closed and flushed the file.
3. `expected_size_bytes` is compared before the copy, and `expected_digest` after it. A mismatch is refused with `INVALID_INPUT` at that field. `actual` holds what was read.
4. The verified copy is published by exclusive link as `blobs/<sha256>` in staging. It is read-only, has one link, and only the recorder wrote it. A producer path is never linked into the store.

A refusal stages no item and no bytes, and the position stays free, so the handle is still usable. The workspace commit lock is not held during the copy. The handle lock is, so other calls on the same handle wait.

A path that no import root holds is recorded as `availability: missing`, like a `missing` source. No empty file stands in for it. A later identical request returns the same `missing` receipt even if the file appears. Attach the file under a new key to retain it.

What the checks cannot see: a writer that stops for the whole copy and writes again afterwards. Declare `expected_size_bytes` or `expected_digest` to close that gap. An identical retry returns the first receipt without reading the file again, so it does not describe bytes written to the path later.

Attaching runs no parser, extractor or command, and does not start, cancel or rerun anything.

## `capture finalize`

```sh
investigation --workspace ./cases capture finalize --input finalize.json --format json
```

`--input` takes a file, or `-` for stdin. The request is the JSON form of `FinalizeCapture`:

```json
{
  "handle": "handle_…",
  "key": "run-0001/finalize",
  "expected_last_position": 4,
  "completion": "partial",
  "execution": {"state": "failed", "exit_code": 3, "error": "worker exited"},
  "channels": [{"channel": "trace", "completeness": "complete"}],
  "declared_artifacts": ["result", "trace"],
  "assertions": [{"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms"}],
  "controls": [{"name": "steady-load", "producer_result": "reported_pass"}],
  "receipts": [{"kind": "signed-statement", "reference": "urn:example:statement:1"}],
  "limitations": ["Single synthetic workload"]
}
```

| Field | Meaning |
|---|---|
| `expected_last_position` | The last position the producer believes was accepted, or `null` if it sent no item. |
| `completion` | `complete` (every item was sent), `partial` (some are known to be absent) or `interrupted` (the producer cannot vouch for its stream). |
| `execution` | `state` (`declared`, `running`, `completed`, `failed`, `cancelled`, `blocked`, `unknown`), optional `exit_code`, `started_at`, `ended_at` and `error`. The error has 1–16384 characters and is kept verbatim. |
| `channels` | The producer's completeness claim per channel, each channel once, at most 256. |
| `declared_artifacts` | Item keys of every artifact the producer meant to attach, each listed once. By default, an unattached key is recorded as missing, and a key of an appended fact is refused. |
| `strict_artifact_inventory` | Optional boolean, default `false`. Set to `true` with `completion: complete` to require an exact match between declared keys and acknowledged attachment keys. |
| `assertions`, `controls` | The envelope's types and bounds. `validity_claim` may only be `producer_reported`. |
| `receipts` | At most 32. `kind`, plus `artifact` (the key of an attached or declared artifact), `reference`, or both. An optional `format` (`in_toto_dsse` or `rfc3161`) asks for a structure check of the attached bytes; see [Executor receipts and chronology](#executor-receipts-and-chronology). |
| `limitations`, `metadata` | As in the envelope. The serialized request is at most 512 KiB. |

Stdout is the `FinalizeReceipt` (`handle`, `run_id`, `case_id`, `finalization`, `execution`, `sealed_position`, `missing_positions`, `manifest_digest`, `commit`), the same value `Recorder::finalize` returns.

The cursor:

- `complete` requires `expected_last_position` to equal the last accepted position. Any other value is refused with `REVISION_CONFLICT` at `/expected_last_position`; `expected` holds the `last_position`. Nothing is sealed, so the handle still accepts items.
- `partial` refuses a cursor behind the last accepted position the same way. A cursor ahead of it names the positions that never arrived in `missing_positions`.
- `interrupted` accepts any cursor and names missing positions the same way.

`finalization` is `finalized` for a complete claim and `finalized_partial` otherwise. It describes how the stream was sealed. A failed execution can be `finalized`, and a completed one can be `finalized_partial`.

### Strict artifact inventory

Set `"strict_artifact_inventory": true` in a complete finalize request to compare `declared_artifacts` with all acknowledged attachment keys under the handle lock. List keys in any order. Include attachments acknowledged with `availability: missing`. Appended facts are outside the artifact inventory. The regular cursor check still applies.

A mismatch returns exit status 4 with `REVISION_CONFLICT` at `/declared_artifacts` and `commit_status: not_committed`. Both arrays in `error.actual` are sorted by key.

- `missing` lists declared keys without an acknowledged attachment.
- `unexpected` lists acknowledged attachment keys absent from the declaration.

The handle stays open with its original receipts. Run the provided `capture recover` repair command to inspect them, then correct the inventory and retry with the same finalize key. After a lost response, resend the identical request, including the strict flag.

Omitting the flag or setting it to `false` keeps the usual inventory rules for all completion values. Finalize retains undeclared attachments and records unattached declarations as missing. Setting the flag to `true` with `partial` or `interrupted` returns `INVALID_INPUT` at `/strict_artifact_inventory`.

### What finalize records

Under the handle lock, finalize seals the handle, then commits one transaction, `capture.finalize` in the journal, attributed to the actor that declared the handle:

- A Source for the capture, with acquisition channel `producer`.
- An Artifact for each attachment with retained bytes. A derivative records its transform: the input is the original's Artifact, and `derived_from.transform` splits at its last `/` into transformation and version (`unspecified` when it has none).
- The manifest Artifact, role `manifest`. Its bytes are an `inv.capture-manifest.v1` document: the stream seal, the case snapshot before finalization, execution, retained and missing artifacts, per-channel completeness, the producer's statements, and every staged item with its position, key, operation, request digest, acceptance time, request and outcome. That item map lets an identical retry resolve after staging is retired, and keeps every fact, including the exact text of an `error` fact. The manifest names neither itself nor its own transaction.
- The next revision of the Run: `execution_status` as reported, `evidence_completeness` as computed, `manifest_digest`, `started_at` and `finished_at` when reported, the new Artifacts appended to `artifact_refs`, and the section `capture_finalization`. The event binds the manifest digest. Every retained byte is copied into the canonical blob store before the event; staging keeps its copies.

Finalize refuses, before sealing, a manifest whose `items` map does not equal the staged items.

The Run body's `capture` binding keeps the `metadata` sent to `begin`, exactly as sent. It is omitted when `begin` sent none, and on Runs declared before it was stored.

`capture_finalization` holds `attribution: producer_reported`, the finalize `command_id`, the begin `producer` and `external_run_id`, `manifest` (`source_ref`, `artifact_ref`, `digest`, `size_bytes`), `snapshot`, `completion`, `expected_last_position`, `sealed_position`, `missing_positions`, `finalization`, `execution`, `evidence_completeness`, `artifacts`, `channels`, and the producer's `assertions`, `controls`, `receipts`, `limitations` and `metadata` exactly as sent. The names follow the `producer_run` section of an envelope Run where both describe the same thing. Each `artifacts` entry has `key`, `position`, `role`, `channel`, `media_type`, `declared` and `retention`: `retained` with `source_ref`, `artifact_ref`, `digest`, `size_bytes` and, for a derivative, `derived_from`; or `missing` with `cause` (`declared`, `not_found` or `not_attached`) and the producer's `reason`.

Completeness states what survived, never more than the producer claims:

- A channel is every channel the request declares or a staged item names. It is `missing` when nothing on it was staged, or when none of its attachments kept bytes. It is `partial` when an attachment on it is missing, it reported a `gap`, or it was never `closed` while the stream is unvouched (`completion` is not `complete`, or positions are missing). Otherwise it is `complete`. The recorded value is the lower of that and the producer's claim; `claimed` keeps the claim.
- The Run's `evidence_completeness` is `missing` when no declared or attached artifact kept bytes, `partial` when any artifact is missing, the stream is unvouched or a channel is below `complete`, and otherwise `complete`. A handle with no items at all is `complete` only for a `complete` claim with no channels.

Execution and evidence are independent. The Run's `evidence_validity` stays `unknown`: an exit code of 0, a `completed` state or a `reported_pass` control sets none. Finalize creates no Observation, Assessment or Conclusion, and writes no verdict. Assertions stay attributed producer claims.

### Seal, retries and crashes

The seal is `sealed.json` in staging. It binds the key, the request digest, the sealed position, the case snapshot and the manifest digest, and its presence closes the handle to new items. Finalize writes it before the transaction:

- A stale cursor, a strict inventory mismatch, an unknown key or an invalid field seals nothing.
- A crash after the seal and before the transaction leaves a sealed handle. The identical request completes it with the same manifest. Any other request is refused with `REVISION_CONFLICT` at `/handle` (or `IDEMPOTENCY_KEY_REUSED` at `/key` for the same key); `expected` names the sealing `command_id` and `request_digest`.
- After the transaction, the identical request returns the original receipt with `commit.result` `already_committed`, even after staging is retired. The same key with a different request is refused with `IDEMPOTENCY_KEY_REUSED` at `/key`. Another key is refused with `REVISION_CONFLICT` at `/handle`; the repair argv looks up the original receipt. A key that names another transaction of the declaring actor, such as the begin key, is refused before sealing.

After the seal, finalize removes `blobs/*.tmp` and any staged blob that no item references. It never removes an item, a key entry or a referenced blob.

## `capture recover`

```sh
investigation --workspace ./cases capture recover --format json
investigation --workspace ./cases capture recover --handle handle_… --format json
```

Without arguments, recover reports every handle that is not finalized. With `--handle` it reports that handle in any state. `--input` (a file, or `-` for stdin) takes the JSON form of `RecoverScope` instead: `{"kind": "unsettled"}` or `{"kind": "handle", "handle": …}`. Stdout is the `RecoveryState`, the same value `Recorder::recover` returns, except that the CLI adds `--workspace` to each next step's `argv`, as it does for repair guidance. `--read-only` allows it.

Each entry in `handles` holds:

| Field | Meaning |
|---|---|
| `handle`, `run_id`, `case_id`, `producer`, `external_run_id` | The declaration, from the begin request. |
| `seal` | `open` (accepting items), `sealed` (`sealed.json` is published and no `capture.finalize` is committed), or `finalized`. |
| `staging` | `present`, `unallocated` (a crash after the declaration left no staging; resend the identical `begin`), or `retired` (finalized and the directory is gone). `staging_dir` is set while staging is present. |
| `last_position` | The last accepted position. After finalization it is the receipt's `sealed_position`, also when staging is retired. |
| `items`, `artifacts` | The receipt of every staged fact and every staged artifact, in position order, with its original position, key, request digest and acceptance time. `result` is `already_accepted`: an earlier call made the item durable. Each artifact reports `availability` `staged`, with its digest and size, or `missing`, as its original receipt did. Read from staging, or from the finalized manifest Artifact once staging is retired. |
| `finalized` | The original finalize receipt, read from the journal, as an identical retry returns it (`commit.result` `already_committed`). Present also after staging is retired. |
| `execution` | The external execution state as recorded: `unknown` until a finalize commits the producer's report, then that report. Recover never infers it from lifecycle facts or from the absence of a record. |
| `next_position` | The position the next item must take, while the handle is open. |
| `next` | Each next step: `action`, the exact `argv` (a request is read from stdin), and `key` and `request_digest` when only one request fits. |

The next steps by state:

| State | `next` |
|---|---|
| Open, staging unallocated | `resend_begin` with the begin `key`. |
| Open, staging present | `append` and `attach` at `next_position`, and `finalize`. |
| Sealed | `resend_finalize` with the sealing `key` and `request_digest`. Only that identical request completes the handle; it commits the manifest the seal names. |
| Finalized | `lookup_receipt`: `investigation receipt KEY --actor ACTOR`. |

Every step records, reads or resends a recording call. No step reruns, resumes or schedules the producer's action, and recover cannot tell whether that action happened: a missing record is not proof that no external effect occurred. A producer that cannot vouch for every step it took finalizes with completion `interrupted` and execution state `unknown`. The Run then records what survived, `finalized_partial`, and keeps every staged byte.

Recover is a read. It takes no handle lock and creates, changes or removes nothing: no seal, item, key entry, lock file, staged blob or temporary, and no index refresh. It reads the journal, then the seal, then the items, so a sealed handle's items are final and an open handle's items are a durable prefix. An item accepted during the read appears in the next one.

A handle that was never declared is refused with `UNKNOWN_REFERENCE` at `/handle`. Refusals that depend on what a handle accepted carry `capture recover --handle …` as their repair argv: a position conflict (`/position`), a reused item key (`/key`), a sealed handle (`/handle`), a stale finalize cursor (`/expected_last_position`), and a handle sealed by another finalize request.

## Late items after sealing

A finalized manifest is immutable. Accepting a late item quietly would falsify it, and dropping the item would lose evidence. The recorder does neither.

**Closed-handle response.** Once `sealed.json` is published, `append` and `attach` refuse every new item with `REVISION_CONFLICT` at `/handle`, whatever its position. The repair argv is `capture recover --handle …`. `actual` holds the item's `key`, `position`, `operation` and `request_digest`. `expected` names what closed the handle:

| `expected.seal` | Other fields |
|---|---|
| `sealed` (no `capture.finalize` committed yet) | `command_id` and `request_digest` of the sealing finalize request, `sealed_position`, and the `manifest_digest` that request will commit. |
| `finalized` | `command_id` of the finalize transaction, `sealed_position`, `manifest_digest`, `manifest_ref` (the manifest Artifact revision), `case_id` and `run_id`. |

**The race with sealing.** Finalize seals while it holds the handle lock, and an item checks the seal while it holds the same lock. If the item wins, it is durable before the seal; finalize then includes it, or refuses a stale `complete` cursor without sealing. If finalize wins, the item gets the closed-handle response. No acknowledged item is left out of the manifest.

**Linked reconciliation.** Late evidence is preserved as a new attributed record linked to the Run, never written into the manifest:

1. Ingest the late bytes into the Run's case with an `inv.ingest.v1` manifest that sets `case_id`. This records a new Source with its own acquisition time, attributed to the submitting actor.
2. Link the new Artifact with `run.reconcile`: the Run's current outcome with the Artifact appended to `artifact_refs`, and a `rationale` that says why it arrived late.

`run.reconcile` keeps the Run's `capture_finalization` section unchanged and refuses any change to `manifest_digest` on a finalized capture (`GATE_FAILED` at `/outcome/manifest_digest`). On a Run with a terminal execution state and `complete` evidence it accepts further artifact references and nothing else. The late Artifact is not in `capture_finalization.artifacts`, so a reader can tell it from what the capture retained.

**After staging retirement.** The manifest's `items` map binds every accepted item: position, key, operation, request digest, acceptance time, request and outcome. When staging is gone, an identical `append` or `attach` retry is answered from that map with its original receipt (`already_accepted`, the original position and `accepted_at`). The same key with a different request is refused with `IDEMPOTENCY_KEY_REUSED` at `/key`, and a new item gets the closed-handle response. None of these writes anything. Resending the identical `begin` for a finalized handle returns its receipt and allocates no staging.

## Staging

```text
state/staging/<run-id>/          0700
  handle.json                    0400, binds the directory to its handle
  handle.lock                    per-handle lock; moved and removed only by retirement
  items/<position>.json          0400, one accepted item; twelve-digit position
  keys/<sha256-of-key>.json      0400, item key to position
  blobs/<sha256>                 0400, verified bytes of one or more attachments
  sealed.json                    0400, written by finalize; closes the handle
```

`staging_dir` in the receipt is this directory, relative to the workspace root. Begin commits the declaration first and then allocates the directory, under the workspace commit lock and with every file and directory synced. `handle.json` holds `schema_version` (`inv.capture-staging.v1`), `handle`, `run_id`, `case_id`, `transaction_id`, `command_id`, `actor`, `producer` and `external_run_id`. Its bytes are a function of the committed declaration only. A retry verifies them, and refuses to overwrite a file bound to another declaration.

Later capture operations add their own entries beside `handle.json` and never rewrite it. A transient `handle.json.tmp` exists only while the binding is published.

An item file holds `schema_version` (`inv.capture-item.v1`), `operation` (`capture.append`, or `capture.attach` for an attachment descriptor), `handle`, `run_id`, `position`, `key`, `request_digest`, `accepted_at`, the normalized `request`, and an optional operation-specific `outcome`. `request_digest` is the SHA-256 of the request's compact JSON with sorted keys. A key file holds `schema_version` (`inv.capture-key.v1`), `key` and `position`. Every accepting call first restores the key file of the last item, which a crash may have left unpublished. An attachment item's `outcome` holds `availability` (`staged` or `missing`), `classification`, and either `digest`, `size_bytes` and `blob` (the path relative to the staging directory) or `missing` (`declared` or `not_found`). A derivative's outcome adds `derived_from` with the original's key and position. A blob is durable before the item that names it. A crash between the two leaves an unreferenced blob, which a retry reuses after checking its bytes. Finalization copies referenced blobs into the canonical blob store. Transient `*.tmp` files exist only while one entry is published or one copy is verified. `sealed.json` is the seal that finalize publishes; see [`capture finalize`](#capture-finalize). Reading the items checks that positions are contiguous, keys are unique, and each request matches its digest.

Staging is durable recovery material, not cache. No cleanup path removes it: publication-temporary reclamation reads only `state/tmp`. A crash after the commit and before allocation leaves a declared handle without staging. `capture recover` reports it as `unallocated`, and resending the identical `begin` allocates the directory. The canonical binding is on the Run record under `capture` (`protocol`, `handle`, `producer`, `external_run_id`, `limits`, `staging`, and `external_plan_digest` when given), so staging can always be checked against the journal.

### Retirement

Staging is retired only by `investigation repair staging [--handle HANDLE]` (Rust: `Store::retire_staging`). Nothing else removes it. Under the handle lock, it removes a handle's staging only when all of these hold:

- The Run is finalized in the verified journal. The finalizing event file and its directory are synced first.
- The manifest Artifact verifies against its digest and binds every staged item exactly.
- Every blob the finalization recorded and every staged attachment's bytes verify by digest in the canonical blob store, and are synced.
- The directory holds only entries the store writes (the layout above and its `*.tmp` files).

Otherwise the staging is kept, and the report gives `result` `kept` with a `reason`: `open` (the handle accepts items), `sealed` (sealed, finalization not committed: resend the sealing finalize), `unallocated` (resend the identical begin), `busy` (another call holds the handle), `unpublished` (the canonical store lacks or contradicts some bytes), or `inconsistent` (the staging disagrees with its declaration or manifest, or holds a foreign entry). Without `--handle`, the report lists every handle that still has staging or was never allocated; with it, that handle in any state, `already_retired` once its staging is gone.

Retirement first renames the whole directory to `state/tmp/retire-<run-id>` and syncs both parents; that rename is the commit point. Only then does it remove the moved entries. A crash before the rename leaves the full staging; a crash after it leaves no staging, and the next retirement removes what is left under `state/tmp`. Publication-temporary reclamation never touches `retire-` directories. Retirement is idempotent: retiring again changes nothing. After it, `capture recover` reports `retired`, and the manifest answers identical `append`, `attach` and `finalize` retries.

`investigation check` verifies staging without changing it. Each `handle.json` must be the exact bytes its declaring transaction allocates (`STAGING_BINDING_MISMATCH`, an error, names both digests), and a seal on a finalized Run must bind the Run's manifest digest (`STAGING_SEAL_MISMATCH`). Its report counts staging under `staging` (`active`, `sealed`, `unallocated`, `retirable`, `retired`, `orphaned`) and warns with a repair command where one applies: `STAGING_RETIRABLE` and `STAGING_RETIREMENT_INCOMPLETE` (`repair staging`), `CAPTURE_SEALED_UNCOMMITTED` and `CAPTURE_STAGING_UNALLOCATED` (`capture recover --handle`), and `STAGING_ORPHAN` for a directory bound to no declared handle, which is never removed automatically.

## Rules every producer follows

- **Keys.** Every mutating call carries an idempotency key. `begin` and `finalize` keys are journal command IDs. `append` and `attach` keys are item keys, unique within the handle. Derive keys from your own run state, never from the clock or a random source at retry time.
- **Retries.** After a timeout, a crash, or a failure whose `commit_status` is `unknown`, resend the identical request with the same key. You get the original receipt. The same key with different content is a conflict.
- **Positions.** Appended facts and attachments share one sequence per handle, starting at 1. The recorder accepts only the next position, and refuses any other with the expected position. After a restart, resend the identical `begin` to get the same handle, call `recover`, and continue after `last_position`. Do not repeat an external action that a recorded step describes. If you cannot vouch for a step that did not finish, finalize with completion `interrupted` and execution state `unknown`.
- **Stable bytes.** Close and flush a file before you attach it, and keep it beneath an import root. Do not write into the staging directory.
- **Assertions.** Your results are recorded as your assertions. An exit code of 0 or a `reported_pass` control sets no measurement validity.
- **Readiness.** If your policy requires durable recording and `begin` fails, do not start a new effectful unit. If the recorder fails midrun, keep a bounded local spool and reconcile later. These are producer obligations; Investigation does not enforce them.

## One-shot envelope `inv.producer-run.v1`

A producer that has already finished can describe the whole run in one JSON document instead of calling the capture operations. `investigation ingest --input ENVELOPE` recognizes it by `schema_version`. Rust callers use `Store::ingest_producer_run`. `investigation schema producer-run` returns the JSON Schema and the schematic example below. The example is relative to a fixture directory ([fixtures/producer-run/v1](../fixtures/producer-run/v1)). Ingest resolves `path` beneath the configured `import_roots`.

```json
{
  "schema_version": "inv.producer-run.v1",
  "producer": {"name": "example-benchmark", "version": "0.1.0", "instance": "local-a"},
  "external_run_id": "bench-run-002",
  "intent": {"label": "compare two queue settings", "external_plan_digest": null},
  "execution": {"state": "completed", "exit_code": 0, "started_at": null, "ended_at": null, "clock_quality": "unknown"},
  "environment": {"revision": "build-abc123", "workload": "fixture-small", "machine": "controlled-fixture"},
  "artifacts": [
    {"role": "original-output", "path": "./bench-result.json", "expected_sha256": null, "media_type": "application/json", "classification": "internal"}
  ],
  "assertions": [{"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms", "validity_claim": "producer_reported"}],
  "controls": [{"name": "steady-load", "producer_result": "reported_pass"}],
  "receipts": [],
  "limitations": ["Single synthetic workload; representative production profile not established"]
}
```

| Field | Required | Meaning |
|---|---|---|
| `producer` | yes | `name` (1–128 ASCII letters, digits, `.`, `_`, `-`), optional `version` and `instance`. Self-reported attribution. |
| `external_run_id` | yes | The producer's stable ID for this attempt. With `producer.name` it identifies the attempt. |
| `intent` | yes | `label` (1–256 characters) and an optional `external_plan_digest`, retained unchecked. |
| `execution` | yes | `state` (`declared`, `running`, `completed`, `failed`, `cancelled`, `blocked`, `unknown`), optional `exit_code`, `started_at`, `ended_at` (verbatim text; RFC 3339 recommended), `clock_quality` (default `unknown`) and `error`. |
| `environment` | no | String map of reported facts, at most 64 entries. Keys follow the `producer.name` rule. |
| `artifacts` | no | At most 32. `role`, relative `path`, optional `expected_sha256` (64 lowercase hex), `media_type`, `classification` (default `internal`). |
| `assertions` | no | At most 256. `kind`, `name`, `value` (any JSON up to 16 KiB), optional `unit`. `validity_claim` may only be `producer_reported`. |
| `controls` | no | At most 64. `name` and `producer_result`, such as `reported_pass`. |
| `receipts` | no | At most 32. `kind` plus an `artifact` (the `path` of an entry in `artifacts`) or a `reference` locator, or both. An optional `format` (`in_toto_dsse` or `rfc3161`) asks for a structure check of the retained bytes; see [Executor receipts and chronology](#executor-receipts-and-chronology). |
| `limitations` | no | At most 64 statements. |
| `classification` | no | Level of the envelope and the Run, default `internal`. The recorded level is the highest of this and every artifact's level. |
| `metadata` | no | Inert namespaced metadata, allowed at the top level and on each artifact, assertion, control and receipt. It is subject to the opaque-metadata bounds. |

Unknown fields are errors. Producer extensions go in `metadata`. An envelope larger than 512 KiB is refused. An unsupported version of the family, for example `inv.producer-run.v2`, is refused with a structured error. To keep such bytes, ingest the file through an `inv.ingest.v1` manifest. It is then retained opaquely with a diagnostic.

### What ingest records

One transaction records the following:

- A minimal `record`-mode case titled with `intent.label`, when `--case` is not given.
- A Source and an Artifact for the envelope bytes, with acquisition item `envelope`. Every assertion, control and receipt is therefore retained source content that a later Observation can cite with a JSON selector.
- A Source and an Artifact for each retained artifact, with item `artifact:` plus the path without its `./` prefix.
- One Run. Its `producer_run` section holds the producer's statements exactly as sent, with `attribution: producer_reported`. It also holds the computed `retention` of each declared artifact (`retained` with exact IDs and digest, `missing`, or `rejected` with the actual digest) and the envelope's IDs.

The Run's `execution_status` is the reported state. Its `evidence_completeness` is computed from retention: `complete` when every declared artifact was retained, `partial` when some were, and `missing` when none of a nonempty inventory were. Its `evidence_validity` is `unknown`. An exit code of 0, a `completed` state or a `reported_pass` control does not change that. Ingest creates no Observation, Assessment or Conclusion. Only an attributed `measurement.assess`, Assessment or Conclusion can interpret the producer's result. The Run's `producer` is the submitting actor, and `tool` names the producer.

A missing file or an `expected_sha256` mismatch affects only that item. The item reports `missing` or `rejected`, its bytes are not retained, and the receipt carries a warning. A path with `..`, an absolute path, a symlink, a file with more than one hard link, or a file over the `ingest.file_bytes` limit (64 GiB) fails the whole request. Nothing is committed.

### Identity and resubmission

The submitting actor comes from `--actor` or the workspace settings. The command ID is derived from the request content, so an identical resubmission returns the original receipt. The attempt follows the acquisition identity rules in [the v1 contract](schema-v1.md):

- The same content from another actor, or after an investigator reconciled the Run, records nothing. Every item reports `deduplicated`.
- A changed envelope revises the same Run and the envelope Source. A changed artifact declaration revises that artifact's Source. Changed bytes add a retained Artifact. Each one reports `revised`. Investigator-owned Run fields, such as `evidence_validity` and the declaration sequence, are carried forward. Earlier artifact references stay on the Run.
- An artifact that was retained earlier and is missing or rejected now keeps its retained entry when its bytes still satisfy `expected_sha256`.
- An attempt belongs to the case that first recorded it. Changed content without `--case` for that case is refused at `/external_run_id`.
- A changed envelope cannot replace a terminal execution status (`completed`, `failed`, `cancelled` or `blocked`) that the Run records. This includes a status that an investigator set with `run.reconcile`. The envelope is refused with `GATE_FAILED` at `/execution/state`. The error names the Run and the recorded status. `run.reconcile` applies the same rule. A status of `declared`, `running` or `unknown` can change to any status. A different outcome is a separate attempt, so give it its own external run ID.
- An attempt that `capture.begin` already declared is refused. See [Attempt identity](#attempt-identity).

The receipt (`producer-run-receipt`) adds `case_id`, the `run` revision, `run_outcome`, an `envelope` item, one item per declared artifact (`imported`, `deduplicated`, `revised`, `missing` or `rejected`), the reported `execution`, the computed `evidence_completeness`, and the Run's `measurement_validity`.

### Relation to the capture operations

`ProducerAssertion` and `ProducerControl` are the same types in the envelope and in `FinalizeCapture`. `ExternalReceipt` has the fields of the envelope's receipt, but names its bytes by an attached item key instead of a path. The envelope's `producer` fields match `ProducerIdentity`, and both follow one name rule.

### Producer bundles

`investigation export --kind producer-bundle` hands recorded runs back out in the same contract: per run an `inv.producer-run.v1` envelope (the original bytes for an ingested run, one built from the finalization for a captured run), the retained artifact bytes and a `producer-bundle.json` index. `investigation ingest --input DIR/producer-bundle.json` re-imports it into any workspace that has DIR beneath an import root, under the attempt identity below. See [Producer bundles](schema-v1.md#producer-bundles). A public bundle, or one exported with `--remap-ids`, is review only, with no envelopes or bytes, because remapping would rewrite identifiers inside them.

## Executor receipts and chronology

A Run's `chronology` says how its frozen plan is known to precede its execution (plan §6.5). A producer can carry receipts in `receipts[]` of `capture finalize` or of an `inv.producer-run.v1` envelope. The receipt bytes are an attached or declared artifact and are retained as an Artifact of the Run, like any other artifact. Every receipt stays the producer's attributed claim.

### Formats

A receipt is parsed only when it declares a `format` and its bytes were retained. No format is inferred from the bytes, the `kind` or a media type. The parser reads only the fields that hold the bound subject digest, and its bounds are the `receipt.*` rows of the [raw-file limits](support-matrix.md#raw-file-limits).

| `format` | Structure that is checked | Subject digests read |
|---|---|---|
| `in_toto_dsse` | A [DSSE envelope](https://github.com/secure-systems-lab/dsse/blob/master/envelope.md): a JSON object with a `payload` string (standard or URL-safe base64, as DSSE allows), a `payloadType` string, and a nonempty `signatures` array whose entries each have a base64 `sig`. Per the [in-toto envelope layer](https://github.com/in-toto/attestation/blob/main/spec/v1/envelope.md), `payloadType` must be `application/vnd.in-toto+json` or `application/vnd.in-toto.<predicate>+json`. The decoded payload must be an [in-toto Statement](https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md) with `_type` `https://in-toto.io/Statement/v1` (or the earlier `https://in-toto.io/Statement/v0.1`), a string `predicateType`, and a nonempty `subject` array in which every entry has a nonempty `digest` set. | The `sha256` value of each subject's `digest`, 64 hexadecimal digits. Other algorithms are kept out of the comparison. The predicate is not interpreted. |
| `rfc3161` | A DER [RFC 3161](https://www.rfc-editor.org/rfc/rfc3161) `TimeStampToken`, or a `TimeStampResp` (§2.4.2) whose status is `granted` or `grantedWithMods` and which holds one. The token is a CMS `ContentInfo` ([RFC 5652](https://www.rfc-editor.org/rfc/rfc5652) §3) of type `id-signedData` (§5.1). Its `EncapsulatedContentInfo` (§5.2) has `eContentType` `id-ct-TSTInfo` and a primitive `eContent` holding the DER `TSTInfo`, and `signerInfos` has at least one `SignerInfo`. The `TSTInfo` must be version 1 and carry a policy, a `MessageImprint`, a serial number and a `genTime`. Lengths must be definite and use at most four octets. | The `messageImprint.hashedMessage` when `hashAlgorithm` is `id-sha256` (2.16.840.1.101.3.4.2.1). `genTime` is copied verbatim and is not trusted. |

The formats were checked against those specifications on 2026-11-23.

### What is and is not verified

| Checked | Not checked |
|---|---|
| The declared structure, as above, within the `receipt.*` bounds. | Any signature: no DSSE `sig` or CMS signature is verified. |
| That the stored receipt bytes are the retained Artifact bytes (digest and size, as for every artifact). | The signer's identity, key ID, certificate chain, TSA policy or any trust anchor. |
| Whether a SHA-256 subject digest equals the Run's frozen `plan_digest`. | That the executor actually bound the plan before execution. The in-toto predicate is the executor's claim and is not interpreted. |
| | That `genTime` is the true time, or that a timestamp orders the execution event. |

The highest level this build records is `structure_checked`. `signature_verified` is reserved for a later build; signature verification is in the [deferred register](deferred.md).

### What the Run records

Finalize and ingest add `receipt_checks` to the Run when the request has receipts: one entry per receipt, in request order.

| Field | Meaning |
|---|---|
| `index`, `kind`, `format` | Position in `receipts`, and the receipt's `kind` and `format` as sent. |
| `artifact_ref`, `receipt_digest` | The Artifact holding the retained bytes, when there is one. |
| `verification` | `structure_checked`; `structure_invalid` when the bytes do not have the declared structure; or `not_checked` when no format was declared, no bytes were retained, or the receipt is over `receipt.bytes`. |
| `subject_digests` | The SHA-256 subject digests that were read. |
| `payload_type`, `statement_type`, `predicate_type`, `signatures_present`, `hash_algorithm`, `gen_time` | Copied from a checked receipt, at most 256 characters each. `signatures_present` counts DSSE signatures or CMS signer infos, none of them verified. |
| `binding` | `plan_digest_bound`, `plan_digest_not_bound`, `no_frozen_plan` (the Run pins no frozen plan) or `not_compared` (no SHA-256 subject). |
| `chronology_effect` | `executor_bound` when this receipt supports that level, otherwise `none`. |
| `note` | Why the receipt has this result. |

A receipt that is `structure_invalid`, or that is `structure_checked` and `plan_digest_not_bound`, adds a receipt warning. A receipt never refuses the request: its bytes are retained whatever the check finds. `capture_finalization.receipts` and `producer_run.receipts` keep the producer's statements exactly as sent.

### Chronology levels

| `chronology` | Set when | `verification` |
|---|---|---|
| `retrospective` | The plan was frozen after the Run was declared. | `journal_order` |
| `order_unknown` | No plan, or no order could be established. Envelope Runs start here. | `journal_order` |
| `recorded_before_run_declaration` | The Run pinned an exact frozen plan when it was declared (`capture begin` with `intent.inquiry_plan`, or `run.start`/`run.record` with a plan intent). | `journal_order` |
| `executor_bound` | A Run that is `recorded_before_run_declaration` has a receipt with `format: in_toto_dsse`, `verification: structure_checked` and `binding: plan_digest_bound`. | `structure_checked` |
| `externally_attested` | Never set by this build. It needs independently verifiable evidence that binds both the plan and the ordering of the execution event, which requires verified signatures. | — |

The rules:

- Only a Statement subject equal to the frozen plan digest supports `executor_bound`. A receipt whose subject does not match never changes chronology, nor does any receipt on a Run with another level.
- An RFC 3161 token never changes chronology. A timestamp whose imprint is the plan digest only dates the plan; it records `plan_digest_bound` with `chronology_effect: none`.
- `intent.external_plan_digest` is retained unchecked and is not a frozen plan, so an envelope Run's receipts record `no_frozen_plan`.
- The `ordering_receipt` of `run.record` is recorded as `verification: unverified`, is not parsed, and never changes chronology.

The upgrade records `chronology_verification: structure_checked` on the Run. Chronology is never shown without its verification level: `show` of a Run reports `chronology` with `level`, `verification` and `receipts_checked`; evidence reuse reports `chronology_verification` beside each Run's `chronology`; and the vault Run note has a `Chronology verification` line. A Run without a recorded level shows `journal_order` for the three local levels.

## Attempt identity

A producer name plus an external run ID identifies one logical attempt. The capture operations and the producer-run envelope use the same identity, and one attempt has one Run. One definition in `investigation_core::attempt` holds the rules, and both paths use it:

- **Name rule.** `is_producer_name` defines the producer name: 1–128 ASCII letters, digits, `.`, `_` or `-`, starting with a letter or digit. The recording crate's `ProducerName`, the envelope's `producer.name` and an ingest `acquisition_key.namespace` all use this rule. `:`, `/` and non-ASCII characters are refused. The published v1 envelope and ingest schemas have the pattern `^[A-Za-z0-9][A-Za-z0-9._-]*$`. The capture schemas were published without a pattern, so they state the rule in `description`, and the recorder enforces it.
- **Recorded identity.** `recorded_attempt` reads the attempt of a Run from the fields that the recording stored. For a capture, these are `capture.producer` and `capture.external_run_id`. For an envelope, these are `producer_run.producer.name` and `producer_run.external_run_id`.
- **One Run per attempt.** `capture begin` for an attempt that an envelope already recorded is refused with `IDEMPOTENCY_KEY_REUSED` at `/external_run_id`. The same refusal applies to an envelope for an attempt that `capture begin` already declared, even after the capture was finalized. In both cases, `error.expected` gives the existing `run_id`, `case_id`, `recorded_by` (`capture.begin` or `producer-run`) and the capture `handle` if there is one. The repair argv runs `capture recover --handle` for a capture Run and `show` for an envelope Run. To continue a capture, use its handle. To revise an envelope Run, ingest a changed envelope. To record a separate attempt, use another external run ID. The same external run ID under another producer name is a different attempt.

### Recordings made before the rules were aligned

Earlier builds allowed `:` and `/` in a capture producer name, and allowed one capture Run and one envelope Run for the same attempt. Recorded identities do not change:

- Journals replay and verify as recorded. `ProducerName` deserialization accepts the `:` and `/` names that stored requests and finalization records hold (`is_recorded_producer_name`). Thus `recover`, `finalize`, `append` and `attach` continue to work on such a handle. An identical retry of its `begin` returns the original receipt. `ProducerName::is_current` reports whether a new attempt can use a name.
- A new `begin` with such a name is refused at `/producer/name`. `ProducerName::new` and `FromStr` refuse it in Rust.
- A workspace that already holds both a capture Run and an envelope Run for one attempt keeps both. Each path continues to use its own Run: a resubmitted envelope maps onto the envelope Run, and a `begin` maps onto the capture Run.

The `crates/investigation-recording/tests/fixtures/legacy-attempt-identity` workspace was recorded by the build before this change. The `attempt_identity` tests replay it.

## Compatibility

The crate is pre-1.0 and versioned with the workspace. Its public surface is the `Recorder` trait, `LocalRecorder`, the request, receipt and value types it defines, and the core contract types it re-exports at its root. Store internals are never exposed. Request, receipt and enum types are `#[non_exhaustive]`: build requests with `new` constructors and set optional fields directly, and match enums with a wildcard arm. New fields, variants and trait methods (with default bodies) are additive. A breaking change needs a minor version bump and a migration note in this file. Serialized forms follow the [v1 rules](schema-v1.md): additive optional fields only.

Migration notes:

- **Producer name rule (W07.18).** `ProducerName::new` and `FromStr` no longer accept `:` or `/`. Rename such a producer, for example `example-org:bench/v1` to `example-org.bench.v1`, before it declares a new attempt. Existing recordings with the old name stay readable and can be finalized. See [Attempt identity](#attempt-identity).

## Producer adapter protocol `inv.producer-adapter.v1`

The conformance runner drives every example producer through this executable interface, whichever language and path it uses. An adapter is a thin command-line front on the producer's normal recording code. It is a test interface, not part of the `investigation` binary.

### Invocation

```text
PRODUCER describe
PRODUCER run    --workspace DIR --output-dir OUT --scenario NAME --producer NAME --external-run-id ID [--protocol VERSION] [--fault FAULT]...
PRODUCER resume --workspace DIR --output-dir OUT --scenario NAME --producer NAME --external-run-id ID [--protocol VERSION]
```

- The runner initializes `DIR` with `investigation init DIR --no-vault --import-root OUT` first. The adapter never creates or deletes workspaces.
- The runner sets `INVESTIGATION_BIN` to the `investigation` executable. A JSON-CLI adapter spawns it with an argument array and JSON on stdin, without a shell. A library adapter ignores it.
- `OUT` is an empty directory outside `DIR` that the runner creates and keeps between `run` and `resume`. The adapter writes its output files there, so a resumed run attaches the same paths. It writes nowhere else, reads nothing else from the environment, and uses no network.
- `--producer` and `--external-run-id` set the producer name and external run ID, so two adapters record identical identities. The begin `producer` object is exactly `{"name": NAME}`, with no `version` or `instance`.
- `--protocol` makes `begin` carry `"protocol": VERSION`. Without it, `begin` has no `protocol` field. The runner passes an unsupported version to check the refusal.
- `OUT` is absolute. An attachment's `source.path` is `OUT` joined with the file name, for example `OUT/result.json`.

### Scenarios

A conforming adapter supports all four scenarios: `synthetic-run`, `partial-evidence`, `interrupted-run` and `late-item`. `describe` lists them. The normative requests are in [fixtures/conformance/v1/scenarios](../fixtures/conformance/v1/scenarios). Each file holds the bytes the producer writes to `OUT` and every request in order. `${producer}`, `${external_run_id}`, `${output_dir}` and `${handle}` stand for the flag values and the handle that `begin` returned. An adapter can build typed requests or send the JSON. Its requests must be equal to the file's requests after substitution. `synthetic-run`:

| Step | Call | Key | Content |
|---|---|---|---|
| 1 | `begin` | `ID/begin` | Actor `agent:` + producer name; intent label `synthetic run`; new record case titled `Synthetic runs`. |
| 2 | `append` | `started` | Position 1, `started`. |
| 3 | `attach` | `result` | Position 2, `original-output`, `application/json`, file `OUT/result.json`. The file holds exactly `{"metric":"latency.p99","value":18.7,"unit":"ms"}` and a newline. |
| 4 | `append` | `phase-summarize` | Position 3, `phase_changed`, phase `summarize`. |
| 5 | `finalize` | `ID/finalize` | Expected last position 3, `complete`, execution `completed` with exit code 0, one `reported_metric` assertion `latency.p99` = 18.7 ms. |

The other scenarios have the same `begin` request, the same keys `ID/begin` and `ID/finalize`, and the same `OUT/result.json`:

| Scenario | Items, then finalize |
|---|---|
| `partial-evidence` | `begin` adds `metadata`. 1 `started` with metadata. 2 `result` on channel `results`, with `expected_digest`, `expected_size_bytes` and metadata. 3 `trace`, role `log`, a `missing` source on channel `trace`. 4 `trace-gap`, a channel `gap` fact. Finalize: `complete` at 4, claims both channels `complete`, declares `result`, `trace` and the never-attached `summary`, reports a control `warm-cache` as `not_run`, and has a receipt, a limitation and metadata. |
| `interrupted-run` | 1 `started`. 2 `result`. Finalize `interrupted` at expected last position 4, execution `unknown`, declares `result`. |
| `late-item` | All of `synthetic-run`, then append `late-note` at position 4 after finalization. The recorder must refuse it with `REVISION_CONFLICT`. The adapter logs the refusal and exits 0. If the item is accepted, the adapter exits 1. |

Requests carry no wall-clock times or random values, so a rerun or resume sends identical requests. The synthetic "action" fabricates the result bytes; it performs no real external action.

### Faults

`--fault` may repeat. `OP` is `begin`, `append`, `attach` or `finalize`; `N` counts calls of that operation from 1.

- `drop-response:OP:N` discards the response to the Nth call and resends the identical request with the same key, as after a lost acknowledgement. `calls` logs both: the first with the result the recorder returned, the resend with `already_accepted` or `already_committed`.
- `crash-after:OP:N` exits with status 75 as soon as the Nth call returns, before the adapter acts on the response.

N counts the scenario's requests of that operation, not resends.

`run` sends every request of the scenario, in order, and does not read `recover` first. Running the same scenario again with the same identity resends every request and gets every original receipt back. The `rerun-identical` case checks this.

`resume` continues the same run after a crash. It resends the step 1 request unchanged to obtain the same handle, calls `recover`, and continues with the first step after `last_position`. It sends no item at or below `last_position`. When the handle is already finalized, it takes the finalize receipt from `recover` or resends the identical finalize request. It never repeats a completed step's external action. If it cannot vouch for a step that did not finish, it finalizes with completion `interrupted` and execution state `unknown`.

### Output

`describe` prints one JSON object:

```json
{"schema_version": "inv.producer-adapter.v1", "protocols": ["inv.capture.v1"], "scenarios": ["interrupted-run", "late-item", "partial-evidence", "synthetic-run"], "faults": ["drop-response", "crash-after"]}
```

`run` and `resume` print exactly one JSON object on stdout, also after an injected crash or a rejection. Diagnostics go to stderr.

```json
{
  "schema_version": "inv.producer-adapter.v1",
  "scenario": "synthetic-run",
  "external_run_id": "run-0001",
  "outcome": "finalized",
  "run_id": "run_…",
  "handle": "handle_…",
  "calls": [
    {"operation": "capture.begin", "key": "run-0001/begin", "position": null, "result": "accepted", "error_code": null}
  ],
  "finalize": null
}
```

- `outcome` is `finalized`, `crashed` or `rejected`. `run_id` and `handle` are `null` before `begin` succeeds.
- Each entry in `calls` records one call as sent, including resends. `result` is `accepted`, `already_accepted`, `committed`, `already_committed` or `rejected`. `error_code` is the structured error code for a rejection.
- `finalize` is the finalize receipt when the run was finalized, else `null`.

| Exit status | Meaning |
|---|---|
| 0 | The scenario ran as instructed and the run is finalized. |
| 1 | The recorder rejected a call the scenario expected to succeed, or accepted one it expected to be refused. |
| 2 | Usage error, or an unknown scenario or fault. Nothing was called and stdout is empty. A `--producer` value that breaks the producer name rule is not a usage error. It is a rejected `begin`: exit 1, outcome `rejected`, and one `capture.begin` call with error code `INVALID_INPUT`. This holds also when a library refuses the name before it calls the recorder. |
| 75 | An injected `crash-after` fault fired. |

The runner judges conformance from canonical state that it reads through the CLI, not from adapter output alone. The output explains a failure; it does not prove a pass.

## Conformance suite

The suite is data in [fixtures/conformance/v1](../fixtures/conformance/v1), versioned with the protocol (`inv.conformance-suite.v1` for `inv.capture.v1`):

- `scenarios/*.json` holds the requests that each scenario sends.
- `cases/*.json` holds the cases. Each one is a sequence of steps that run in a new workspace: drive the producer (`run` or `resume`, with faults, a protocol or an identity), make a direct CLI call with an expected response, replay every manifest item, or check canonical state.
- `expected/*.json` holds the canonical state of a scenario after normalization, as a pattern.
- `suite.json` maps each §11.7 item and each recording rule (positions, attachments, missing attachments, interrupted runs, late items, finalize retry, recover, attempt identity, protocol version) to the cases that cover it.

```sh
python3 scripts/run-conformance.py --bin target/debug/investigation \
    --producer 'python3 scripts/conformance/reference_producer.py' \
    --producer 'PATH/TO/OTHER-ADAPTER'
```

`--producer` can repeat. `--case NAME` selects cases. `--keep DIR` keeps each workspace. `--dump DIR` writes each normalized state. `--jobs N` sets how many cases run at once (default 8). `--json` prints one report. `--validate-suite` checks only the suite files. The exit status is 0 only when every producer passes every case and, with more than one producer, each producer after the first records the same normalized state as the first.

The runner gets every verdict from the binary, never from the adapter's word:

- It finds an attempt by its begin receipt (`receipt ID/begin --actor agent:NAME`), not by the `run_id` that the adapter prints.
- It reads the Run and the case with `show --depth full` and the handle with `capture recover --handle`. It reads the manifest and every retained artifact with `evidence read` and a `whole` selector. It reads the workspace with `check`: the number of cases, the journal sequence of each, error issues and staging counts.
- Every finalized attempt must satisfy these properties:
  - The manifest bytes hash to the Run's `manifest_digest`.
  - Positions are contiguous from 1, and keys are unique.
  - The `recover` receipts equal the manifest items.
  - Retained bytes hash to their recorded digests.
  - `evidence_validity` stays `unknown`.
  - The finalize receipt updates the Run.
- To normalize state, the runner replaces the workspace, output directory, handle, Run, case, producer name and external run ID with placeholders. It masks generated IDs and drops wall-clock times, journal hashes, request digests and the manifest's own digest and size. The remaining state must match the expected pattern. Extra fields are allowed, so additive v1 fields do not break the suite.

`scripts/conformance/reference_producer.py` is a standard-library producer that sends the scenario files through the JSON CLI. It passes every case. `scripts/conformance/non_idempotent_producer.py` has one defect: it records a step again under a fresh key when a response is lost. The suite fails it on the four lost-response cases. A duplicated item fails the canonical check even when the adapter's call log is ignored. `crates/investigation-cli/tests/conformance.rs` runs both. It also runs the reference producer, the Python example (`examples/python-producer/producer.py`) and the Rust example (`example-rust-producer`) together. All three must pass all 20 cases and record the same normalized state.

`--round-trip` also exports every case of each case's workspace as a producer bundle and re-imports it, judged from canonical state too. Every finalized captured run gets an envelope unless it is recorded less complete than its artifacts show. Re-imported into a fresh workspace, every run is `imported`, `check` passes, and a bundle exported there gives the same facts: producer, intent, execution, environment, completeness, artifact roles, retention and digests, assertions, controls, receipts, limitations and metadata. Re-imported into the source workspace, no run is new and no journal moves. `conformance.rs` runs the reference producer with it.

To prove a new adapter, add `--producer` for it. Its result counts only when it passes every case and its normalized state equals the reference producer's.
