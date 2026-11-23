# Recording API and producer adapter protocol

A producer is an external program that runs something under its own authority and records what happened. It links the `investigation-recording` crate (Rust) or sends JSON to the `investigation` CLI (any language). Both paths use one implementation and one payload shape, protocol `inv.capture.v1`. Investigation never imports, names or invokes a producer.

## Operations

| Operation | Rust call | Does | Does not |
|---|---|---|---|
| `capture.begin` | `Recorder::begin(&BeginCapture)` → `BeginReceipt` | Declares one Run (and, if asked, a minimal `record`-mode case) in one transaction, allocates private staging, returns the handle, IDs, protocol, limits and staging path. | Authorize, approve or start the producer's action; verify the producer's identity or permission. |
| `capture.append` | `Recorder::append(&CaptureEvent)` → `ItemReceipt` | Accepts one bounded lifecycle or collection fact at the next position, durably, with distinct reported and accepted times. | Start, cancel or rerun anything; reorder items or fill gaps. |
| `capture.attach` | `Recorder::attach(&StableArtifact)` → `ArtifactRef` | Copies a closed file into staging and verifies digest, size and role, or records a declared artifact as missing. | Run a parser or command; accept mutable bytes as final; hardlink producer files; replace a missing artifact with an empty one. |
| `capture.finalize` | `Recorder::finalize(&FinalizeCapture)` → `FinalizeReceipt` | Seals the handle and commits the immutable manifest: artifacts, execution state, assertions, controls, receipts, per-channel completeness. | Create an Assessment or a validated Conclusion; equate `finalized` with `execution_completed`. |
| `capture.recover` | `Recorder::recover(&RecoverScope)` → `RecoveryState` | Reads unsettled handles, or one handle: seal state, last accepted position and receipts. | Rerun or resume an external action; delete staged bytes; change a handle's state. |

The crate documentation states the same limits on each method. Run `cargo doc -p investigation-recording --open` to read it.

**Status.** The types and signatures are fixed. `begin` is durable and has its CLI command, `capture begin`. The other `LocalRecorder` calls return `RecordingError::Unsupported` and record nothing. Each operation's durable behaviour and its CLI command land separately, without changing the signatures.

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
- `classification` defaults to `internal`. `restricted` also applies to the Run record.

Stdout is the `BeginReceipt` (`handle`, `run_id`, `case_id`, `protocol`, `limits`, `staging_dir`, `commit`), the same value `Recorder::begin` returns. A rejection prints the structured `inv.response.v1` error with `ok: false`. The Run is `declared`, with evidence completeness and measurement validity `unknown`. Begin records a declaration only: it does not authorize, approve or start anything.

Idempotency:

- The key is the journal command ID. Resending the identical request returns the original receipt with `commit.result` `already_committed` and the same Run, handle and staging. Fields that take a default compare equal whether or not they are spelled out.
- The same key and actor with a different request is refused with `IDEMPOTENCY_KEY_REUSED` at `/command_id`.
- The producer name and external run ID identify one logical attempt. A second `begin` for that pair under another key or actor is refused with `IDEMPOTENCY_KEY_REUSED` at `/external_run_id`. The error names the original command ID, Run and handle, and its repair argv looks up the original receipt.

The limits are fixed when the handle is declared: 65536 bytes per appended event, 67108864 bytes per attachment and 100000 positions. A later build with other defaults returns the original limits on a retry.

## Staging

```text
state/staging/<run-id>/          0700
  handle.json                    0400, binds the directory to its handle
```

`staging_dir` in the receipt is this directory, relative to the workspace root. Begin commits the declaration first and then allocates the directory, under the workspace commit lock and with every file and directory synced. `handle.json` holds `schema_version` (`inv.capture-staging.v1`), `handle`, `run_id`, `case_id`, `transaction_id`, `command_id`, `actor`, `producer` and `external_run_id`. Its bytes are a function of the committed declaration only. A retry verifies them, and refuses to overwrite a file bound to another declaration.

Later capture operations add their own entries beside `handle.json` and never rewrite it. A transient `handle.json.tmp` exists only while the binding is published.

Staging is durable recovery material, not cache. No cleanup path removes it: publication-temporary reclamation reads only `state/tmp`. A crash after the commit and before allocation leaves a declared handle without staging. Recovery reports that state, and resending the identical `begin` allocates the directory. The canonical binding is on the Run record under `capture` (`protocol`, `handle`, `producer`, `external_run_id`, `limits`, `staging`, and `external_plan_digest` when given), so staging can always be checked against the journal.

## Rules every producer follows

- **Keys.** Every mutating call carries an idempotency key. `begin` and `finalize` keys are journal command IDs. `append` and `attach` keys are item keys, unique within the handle. Derive keys from your own run state, never from the clock or a random source at retry time.
- **Retries.** After a timeout, a crash, or a failure whose `commit_status` is `unknown`, resend the identical request with the same key. You get the original receipt. The same key with different content is a conflict.
- **Positions.** Appended facts and attachments share one sequence per handle, starting at 1. The recorder accepts only the next position. After a restart, resend the identical `begin` to get the same handle, call `recover`, and continue after `last_position`.
- **Stable bytes.** Close and flush a file before you attach it. Do not write into the staging directory.
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

The receipt (`producer-run-receipt`) adds `case_id`, the `run` revision, `run_outcome`, an `envelope` item, one item per declared artifact (`imported`, `deduplicated`, `revised`, `missing` or `rejected`), the reported `execution`, the computed `evidence_completeness`, and the Run's `measurement_validity`.

### Relation to the capture operations

`ProducerAssertion` and `ProducerControl` are the same types in the envelope and in `FinalizeCapture`. `ExternalReceipt` has the fields of the envelope's receipt, but names its bytes by an attached item key instead of a path. The envelope's `producer` fields match `ProducerIdentity`. The envelope's name rule is stricter: it does not allow `:` or `/`, because the name becomes the acquisition namespace.

## Compatibility

The crate is pre-1.0 and versioned with the workspace. Its public surface is the `Recorder` trait, `LocalRecorder`, the request, receipt and value types it defines, and the core contract types it re-exports at its root. Store internals are never exposed. Request, receipt and enum types are `#[non_exhaustive]`: build requests with `new` constructors and set optional fields directly, and match enums with a wildcard arm. New fields, variants and trait methods (with default bodies) are additive. A breaking change needs a minor version bump and a migration note in this file. Serialized forms follow the [v1 rules](schema-v1.md): additive optional fields only.

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
