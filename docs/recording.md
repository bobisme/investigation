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

**Status.** The types and signatures are fixed. In this build every `LocalRecorder` call returns `RecordingError::Unsupported` and records nothing. Each operation's durable behaviour and its CLI command land separately, without changing the signatures.

## Rules every producer follows

- **Keys.** Every mutating call carries an idempotency key. `begin` and `finalize` keys are journal command IDs. `append` and `attach` keys are item keys, unique within the handle. Derive keys from your own run state, never from the clock or a random source at retry time.
- **Retries.** After a timeout, a crash, or a failure whose `commit_status` is `unknown`, resend the identical request with the same key. You get the original receipt. The same key with different content is a conflict.
- **Positions.** Appended facts and attachments share one sequence per handle, starting at 1. The recorder accepts only the next position. After a restart, resend the identical `begin` to get the same handle, call `recover`, and continue after `last_position`.
- **Stable bytes.** Close and flush a file before you attach it. Do not write into the staging directory.
- **Assertions.** Your results are recorded as your assertions. An exit code of 0 or a `reported_pass` control sets no measurement validity.
- **Readiness.** If your policy requires durable recording and `begin` fails, do not start a new effectful unit. If the recorder fails midrun, keep a bounded local spool and reconcile later. These are producer obligations; Investigation does not enforce them.

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
