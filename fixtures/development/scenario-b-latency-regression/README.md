# Scenario B: p99 latency regression

All data is synthetic, local and provider-neutral. The subject is a fictional
service ("Ledgerd") on a fictional runtime ("Marlin") with a new allocator
("slab-v2") and a candidate optimization A ("arena-reuse"). The producers
`tidepool-bench` and `tidepool-stats` do not exist. Nothing here names a real
product, and the family shares no data with the A-gate pilot families or any
sealed material.

Question for every variant: after the Marlin runtime upgrade from 1.8 to 1.9,
did the allocator change cause a p99 latency regression, and does optimization
A improve p99 meaningfully? Each variant offers exactly one specific mistake;
`mistake_offered` in its `manifest.json` names it.

## Inputs

Each run directory `<variant>/runs/<run>/` holds:

| File | Role |
|---|---|
| `envelope.json` | `inv.producer-run.v1` envelope from the generic producer. Its artifact is `output.json`, pinned by `expected_sha256`. Producer results are attributed claims, not validity. |
| `output.json` | The producer's raw output (latency values, or per-request records). |
| `measurement.json` | The typed measurement fields (`measurement_fields.rs`) an investigator records for the run: samples (flat or leveled), `trial_order`, `load_model`, `attempts`, environment and tool identity, warm-up, window. Absent means not recorded; `unknown` is explicit. |

Every envelope reports exit code 0. Process exit is not measurement validity.

## Variants

| Variant | Mistake offered |
|---|---|
| `baseline` | Pooling the debug-logging run with the treatment, or comparing optimization A with the treatment although warm-up (30 s vs 5 s) and workload mix (read90-write10 vs read70-write30) differ. |
| `negative-short-window` | Recording "no improvement detected" as equality, after a failed monitor self-check and an 8 s window of a planned 300 s (40 samples). |
| `sequential-order` | Treating one A-then-B comparison as paired evidence; run A's first 60 samples carry a cold-start decay. |
| `closed-loop-omission` | Reporting the actual-send p99 of a closed-loop capture. A 600 ms stall delays every later send; measured from the intended schedule the p99 is far larger. |
| `missing-outcome` | Answering "99% within 25 ms" from the completion-only p99 of 10 ms. 100 started, 95 finished, 5 unfinished at the 1 s timeout: only 95/100 are within 25 ms. |
| `practical-equivalence` | Counting the interval [-8, +3] as inside the prespecified +-5 ms band, or inventing an interval. [-2, +3] supports bounded equivalence; [-8, +3] leaves it unresolved. |

## Generation and checking

`scripts/generate-scenario-b-fixture.py` writes every JSON file from one fixed
seed with integer-only arithmetic (xorshift64*), so output is byte-identical on
every run. `scripts/check-scenario-b-fixture.py` regenerates the family, compares
it to the committed bytes, and evaluates each `expected.json` property from the
data, including recomputing every p99 by nearest rank and re-deriving the
advisory flags from `measurement.json`.

Each `expected.json` property has an `assert` name and plain-data arguments so
the end-to-end test can evaluate it without generated IDs. Flag IDs in
`derived_flags_equal` and `derived_pair_flags_equal` are the core's advisory
`measurement.*` flag IDs (`measurement_flags.rs`, rule `measurement-flags.v1`);
IDs prefixed `fixture.` are conditions the fixture names that the core does not
flag. Each `measurement.json` records a `statistic` block (p99 quantile,
population, combination) so the order-statistic and completion-only flags can fire. Digests are in `digests.json`.

## By-role reading guides

- `roles/investigator.md` gives the order in which to read the inputs.
- `roles/reviewer.md` lists the comparability, ordering and denominator checks.
