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
| `channel` | `channel`, `state` (`opened`, `closed`, or `gap` with optional `detail`) | channel 1–256 characters, no control characters; detail 1–4096 characters |

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
| `declared_artifacts` | Item keys of every artifact the producer meant to attach. A key that was never attached is recorded as missing. A key of an appended fact is refused. |
| `assertions`, `controls` | The envelope's types and bounds. `validity_claim` may only be `producer_reported`. |
| `receipts` | At most 32. `kind`, plus `artifact` (the key of an attached or declared artifact), `reference`, or both. |
| `limitations`, `metadata` | As in the envelope. The serialized request is at most 512 KiB. |

Stdout is the `FinalizeReceipt` (`handle`, `run_id`, `case_id`, `finalization`, `execution`, `sealed_position`, `missing_positions`, `manifest_digest`, `commit`), the same value `Recorder::finalize` returns.

The cursor:

- `complete` requires `expected_last_position` to equal the last accepted position. Any other value is refused with `REVISION_CONFLICT` at `/expected_last_position`; `expected` holds the `last_position`. Nothing is sealed, so the handle still accepts items.
- `partial` refuses a cursor behind the last accepted position the same way. A cursor ahead of it names the positions that never arrived in `missing_positions`.
- `interrupted` accepts any cursor and names missing positions the same way.

`finalization` is `finalized` for a complete claim, otherwise `finalized_partial`. It says how the stream was sealed, not how the action ended: a failed execution can be `finalized`, and a completed one `finalized_partial`.

### What finalize records

Under the handle lock, finalize seals the handle, then commits one transaction, `capture.finalize` in the journal, attributed to the actor that declared the handle:

- A Source for the capture, with acquisition channel `producer`.
- An Artifact for each attachment with retained bytes. A derivative records its transform: the input is the original's Artifact, and `derived_from.transform` splits at its last `/` into transformation and version (`unspecified` when it has none).
- The manifest Artifact, role `manifest`. Its bytes are an `inv.capture-manifest.v1` document: the stream seal, the case snapshot before finalization, execution, retained and missing artifacts, per-channel completeness, the producer's statements, and every staged item with its position, key, operation, request digest, acceptance time, request and outcome. That item map lets an identical retry resolve after staging is retired, and keeps every fact, including the exact text of an `error` fact. The manifest names neither itself nor its own transaction.
- The next revision of the Run: `execution_status` as reported, `evidence_completeness` as computed, `manifest_digest`, `started_at` and `finished_at` when reported, the new Artifacts appended to `artifact_refs`, and the section `capture_finalization`. The event binds the manifest digest. Every retained byte is copied into the canonical blob store before the event; staging keeps its copies.

Finalize refuses, before sealing, a manifest whose `items` map does not equal the staged items.

`capture_finalization` holds `attribution: producer_reported`, the finalize `command_id`, the begin `producer` and `external_run_id`, `manifest` (`source_ref`, `artifact_ref`, `digest`, `size_bytes`), `snapshot`, `completion`, `expected_last_position`, `sealed_position`, `missing_positions`, `finalization`, `execution`, `evidence_completeness`, `artifacts`, `channels`, and the producer's `assertions`, `controls`, `receipts`, `limitations` and `metadata` exactly as sent. The names follow the `producer_run` section of an envelope Run where both describe the same thing. Each `artifacts` entry has `key`, `position`, `role`, `channel`, `media_type`, `declared` and `retention`: `retained` with `source_ref`, `artifact_ref`, `digest`, `size_bytes` and, for a derivative, `derived_from`; or `missing` with `cause` (`declared`, `not_found` or `not_attached`) and the producer's `reason`.

Completeness states what survived, never more than the producer claims:

- A channel is every channel the request declares or a staged item names. It is `missing` when nothing on it was staged, or when none of its attachments kept bytes. It is `partial` when an attachment on it is missing, it reported a `gap`, or it was never `closed` while the stream is unvouched (`completion` is not `complete`, or positions are missing). Otherwise it is `complete`. The recorded value is the lower of that and the producer's claim; `claimed` keeps the claim.
- The Run's `evidence_completeness` is `missing` when no declared or attached artifact kept bytes, `partial` when any artifact is missing, the stream is unvouched or a channel is below `complete`, and otherwise `complete`. A handle with no items at all is `complete` only for a `complete` claim with no channels.

Execution and evidence are independent. The Run's `evidence_validity` stays `unknown`: an exit code of 0, a `completed` state or a `reported_pass` control sets none. Finalize creates no Observation, Assessment or Conclusion, and writes no verdict. Assertions stay attributed producer claims.

### Seal, retries and crashes

The seal is `sealed.json` in staging. It binds the key, the request digest, the sealed position, the case snapshot and the manifest digest, and its presence closes the handle to new items. Finalize writes it before the transaction:

- A refused request (a stale cursor, an unknown key, an invalid field) seals nothing.
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
| `receipts` | no | At most 32. `kind` plus an `artifact` (the `path` of an entry in `artifacts`) or a `reference` locator, or both. |
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

A missing file or an `expected_sha256` mismatch affects only that item. The item reports `missing` or `rejected`, its bytes are not retained, and the receipt carries a warning. A path with `..`, an absolute path, a symlink, a file with more than one hard link, or a file over 64 MiB fails the whole request. Nothing is committed.

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
PRODUCER run    --workspace DIR --output-dir OUT --scenario NAME --producer NAME --external-run-id ID [--fault FAULT]...
PRODUCER resume --workspace DIR --output-dir OUT --scenario NAME --producer NAME --external-run-id ID
```

- The runner initializes `DIR` with `investigation init DIR --no-vault` first. The adapter never creates or deletes workspaces.
- The runner sets `INVESTIGATION_BIN` to the `investigation` executable. A JSON-CLI adapter spawns it with an argument array and JSON on stdin, without a shell. A library adapter ignores it.
- `OUT` is an empty directory outside `DIR` that the runner creates and keeps between `run` and `resume`. The adapter writes its output files there, so a resumed run attaches the same paths. It writes nowhere else, reads nothing else from the environment, and uses no network.
- `--producer` and `--external-run-id` set the producer name and external run ID, so two adapters record identical identities.

### Scenarios

Every adapter supports `synthetic-run`. The conformance suite may add scenarios; `describe` lists the ones an adapter supports.

| Step | Call | Key | Content |
|---|---|---|---|
| 1 | `begin` | `ID/begin` | Actor `agent:` + producer name; intent label `synthetic run`; new record case titled `Synthetic runs`. |
| 2 | `append` | `started` | Position 1, `started`. |
| 3 | `attach` | `result` | Position 2, `original-output`, `application/json`, file `OUT/result.json`. The file holds exactly `{"metric":"latency.p99","value":18.7,"unit":"ms"}` and a newline. |
| 4 | `append` | `phase-summarize` | Position 3, `phase_changed`, phase `summarize`. |
| 5 | `finalize` | `ID/finalize` | Expected last position 3, `complete`, execution `completed` with exit code 0, one `reported_metric` assertion `latency.p99` = 18.7 ms. |

Requests carry no wall-clock times or random values, so a rerun or resume sends identical requests. The synthetic "action" fabricates the result bytes; it performs no real external action.

### Faults

`--fault` may repeat. `OP` is `begin`, `append`, `attach` or `finalize`; `N` counts calls of that operation from 1.

- `drop-response:OP:N` — discard the response to the Nth call and resend the identical request with the same key, as after a lost acknowledgement.
- `crash-after:OP:N` — exit with status 75 as soon as the Nth call returns, before acting on the response.

`resume` continues the same run after a crash. It resends the step 1 request unchanged to obtain the same handle, calls `recover`, and continues with the first step after `last_position`. It never repeats a completed step's external action. If it cannot vouch for a step that did not finish, it finalizes with completion `interrupted` and execution state `unknown`.

### Output

`describe` prints one JSON object:

```json
{"schema_version": "inv.producer-adapter.v1", "protocols": ["inv.capture.v1"], "scenarios": ["synthetic-run"], "faults": ["drop-response", "crash-after"]}
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
| 1 | The recorder rejected a call the scenario expected to succeed. |
| 2 | Usage error, or an unknown scenario or fault. Nothing was called and stdout is empty. |
| 75 | An injected `crash-after` fault fired. |

The runner judges conformance from canonical state that it reads through the CLI, not from adapter output alone. The output explains a failure; it does not prove a pass.
