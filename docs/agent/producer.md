<!-- inv.producer-onboarding.v1 -->
# Record a program run (producer onboarding)

Read this if you write a program that runs something and want its run recorded. You need no investigation method, case or hypothesis. You send JSON to the `investigation` command and read JSON back. The protocol is `inv.capture.v1`. Any language works; use an argument array, never a shell string. The complete contract is in the repository file `docs/recording.md`. This page is enough to build a working producer.

The recorder stores what you report. It never starts, stops, reruns or authorizes your action, and it never names or imports your program. An exit code of 0 or a passing control you report proves nothing about measurement validity.

## 0. Prepare

1. Create a workspace once: `investigation init ./cases --no-vault`.
2. Name the directory that holds your output files an import root: `investigation init ./cases --import-root ./out`. Attached files must lie beneath an import root, or they are recorded as missing.
3. Pick a stable `producer` name (ASCII letters, digits, `.`, `_`, `-`; start with a letter or digit; no `:` or `/`) and an `external_run_id`. The pair identifies one logical attempt and gets one Run. Derive both from your own state.
4. Learn the machine contract: `investigation schema capture --format json` lists the protocol versions, operation names, limits and the schema name of each request. `investigation schema --list --format json` lists every schema name.

Every call below is one `investigation` process with `--workspace ./cases`, `--input -` and `--format json`, with one strict JSON request on stdin (no `NaN`, no `Infinity`, no duplicate keys). Read one JSON object from stdout. `"ok": false` marks a refusal. Its `error` object holds `code`, `path` and often `expected`.

## 1. The five calls

Keys are your idempotency keys. Derive them from your run, for example `RUN/begin`, `started`, `result`, `RUN/finalize`. Never use the clock or a random value in a key or a request, or a retry stops being identical.

### begin

```sh
investigation --workspace ./cases capture begin --input - --format json
```

```json
{"key": "run-0001/begin", "actor": "agent:example-producer",
 "producer": {"name": "example-producer"}, "external_run_id": "run-0001",
 "intent": {"label": "synthetic run"},
 "case": {"kind": "new_record_case", "title": "Synthetic runs"},
 "protocol": "inv.capture.v1", "metadata": {"example-org.fixture": {"seed": 7}}}
```

`protocol` and `metadata` are optional. Use `{"kind": "existing", "case_id": ...}` to join a case. The receipt holds `handle`, `run_id`, `case_id`, `protocol`, `limits` and `commit` (its `result` is `committed`, or `already_committed` on a retry). Keep `handle` and send it in every later call. Begin declares only; it starts nothing. A name with `:` is refused at `/producer/name`.

### append

```sh
investigation --workspace ./cases capture append --input - --format json
```

```json
{"handle": "HANDLE", "key": "started", "position": 1, "fact": {"kind": "started"}}
```

Facts: `started`; `phase_changed` with `phase`; `error` with `message` and optional `external_code`; `cancelled` with optional `reason`; `channel` with `channel` and `state` (`{"kind": "opened"}`, `{"kind": "closed"}` or `{"kind": "gap", "detail": ...}`). Optional `metadata` carries your own namespaced data. The receipt `result` is `accepted` or, on a retry, `already_accepted`. A serialized request may be at most 65536 bytes; put large bytes in an attachment.

### attach

```sh
investigation --workspace ./cases capture attach --input - --format json
```

```json
{"handle": "HANDLE", "key": "result", "position": 2, "role": "original-output",
 "source": {"kind": "file", "path": "/abs/path/out/result.json"},
 "channel": "results", "media_type": "application/json",
 "expected_digest": "sha256:HEX", "expected_size_bytes": 48}
```

`role` is `original-output`, `derivative` (needs `derived_from` with `from` and `transform`), `log` or `receipt`. `channel`, `media_type`, `expected_digest`, `expected_size_bytes`, `classification` and `metadata` are optional; a wrong digest or size refuses the item. For an artifact you could not produce, send `"source": {"kind": "missing", "reason": "..."}`: it is recorded as missing, never as an empty file. Close and flush a file before attaching it, and never write into staging. The response holds `receipt` (with its own `result`) and `availability`; `availability` is `staged` or `missing`.

### finalize

```sh
investigation --workspace ./cases capture finalize --input - --format json
```

```json
{"handle": "HANDLE", "key": "run-0001/finalize", "expected_last_position": 3,
 "completion": "complete", "execution": {"state": "completed", "exit_code": 0},
 "channels": [{"channel": "results", "completeness": "complete"}],
 "declared_artifacts": ["result", "summary"],
 "assertions": [{"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms"}],
 "controls": [{"name": "steady-load", "producer_result": "reported_pass"},
              {"name": "warm-cache", "producer_result": "not_run"}],
 "receipts": [{"kind": "signed-statement", "reference": "urn:example:statement:1"}],
 "limitations": ["Single synthetic workload"], "metadata": {"example-org.fixture": {"seed": 7}}}
```

`expected_last_position` is the last position you believe was accepted (`null` if none). `completion` is `complete` (you sent everything), `partial` (some items are known to be absent) or `interrupted` (you cannot vouch for your stream). The `state` of `execution` is one of `declared`, `running`, `completed`, `failed`, `cancelled`, `blocked`, `unknown`, with optional `exit_code`, `error`, `started_at` and `ended_at`. List in `declared_artifacts` the key of every artifact you meant to attach: a key never attached is recorded as missing. Finalize seals the handle and writes an immutable manifest. It creates no verdict, and `finalized` does not mean the action completed.

### recover (read only)

```sh
investigation --workspace ./cases capture recover --handle HANDLE --format json
```

The response holds `handles`, one entry per handle (without `--handle`, every unfinished handle). Read `seal` (`open`, `sealed`, `finalized`), `last_position`, `items`, `artifacts`, `next_position` and `next`. It reruns nothing.

## 2. Positions and retries

- Appended facts and attachments share one sequence per handle, starting at 1. Only the next position is accepted. Any other gives `REVISION_CONFLICT` at `/position` with the expected position, and stages nothing.
- A timeout, an unparseable response, a crash, or a refusal whose `commit_status` is `unknown` means you do not know if the call committed. Resend the identical request with the same key (three resends are plenty). You get the original receipt: `already_committed` or `already_accepted`.
- The same key with different content gives `IDEMPOTENCY_KEY_REUSED`. A second `begin` for the same producer and external run ID under another key gives the same code at `/external_run_id`.
- A refused attachment (for example `INVALID_INPUT` at `/source/path`) consumes no position. Keep its key in `declared_artifacts` so the manifest names it as missing, and set `completion` to `partial`.

## 3. Restart after a crash

1. Resend the identical `begin`: you get the same `handle`.
2. Call `recover` for that handle.
3. If `seal` is not `open`, resend the identical `finalize` (a sealed handle accepts nothing else) and stop.
4. Otherwise continue after `last_position`. Skip every step already recorded, and do not repeat an external action that a recorded step describes. Write the output once: if the output file already holds its bytes, keep it.
5. If a recorded item is not one you sent, or you cannot vouch for an unfinished step, finalize with `completion` `interrupted`, `execution` `{"state": "unknown"}` and `expected_last_position` set to the recovered `last_position`. A missing record never proves that no external effect happened.

## 4. Late items and failures of the recorder

- After the seal, `append` and `attach` return `REVISION_CONFLICT` at `/handle`. Do not retry them. Late bytes are linked later with `run.reconcile` after an `ingest` of the late file into the case; they never enter the sealed manifest.
- If your policy requires durable recording and `begin` fails, start no new effectful unit. If the recorder fails midrun, keep a bounded local spool of unsent requests and replay them with the same keys when it returns. Investigation does not enforce this; it is your obligation.
- After finalization, `investigation receipt KEY --actor ACTOR` looks up the original receipt.

## 5. Check your producer

Implement the `inv.producer-adapter.v1` interface (`describe`, `run`, `resume`) and run `python3 scripts/run-conformance.py --bin BIN --producer COMMAND` from the repository. The Python example `examples/python-producer/producer.py` and the Rust example `examples/rust-producer` pass it. If you already hold a finished run in the `inv.producer-run.v1` envelope, `investigation ingest --input FILE` records it in one step.

To move an existing journal onto this recorder, read `investigation docs migration`.
