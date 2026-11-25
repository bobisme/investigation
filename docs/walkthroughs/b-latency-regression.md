# Walkthrough B: a p99 latency regression

Scenario B of plan §17.2 is a measurement case. After a runtime upgrade, did
an allocator change cause a p99 latency regression, and does a proposed
optimization ("optimization A") improve p99 by a useful amount? This
walkthrough reads the worked case that `demo` builds. It shows why a
benchmark that exits cleanly is not yet a measurement, and why "no
improvement detected" is not evidence of no effect. See the
[index](README.md) for how the commands are run and checked.

The same binary and operation catalog handle all four walkthroughs; only
optional method cards change. The general `measure`, `baseline` and
`negative-result` cards apply here. The optional
[`performance`](../../methods/performance.toml) pack specializes them.

## Build the case

```sh
investigation demo ./demo-case --scenario performance
```

```text
demo  performance  <WORKSPACE>
fixture  fixtures/vault-goldens/scenario-b  replayed 75 transactions  seed 0x5eed0b17
case  case_01a210e7-b0f7-7001-adab-1a5a11b46bcf
open step  The bounded conclusions await the attributed retrospective and case closure.
...
```

## Read the handoff first

```sh
investigation --workspace ./demo-case brief --level compact
```

```text
case_01a210e7-b0f7-7001-adab-1a5a11b46bcf  active / needs_review  snapshot 75  level compact
...
outcomes  shown 7
...
concl_01a210e8-04a0-7000-a4a0-7478a7bb7a64  candidate / current  revision 1  title [authored_data]: "Optimization A does not change p99."
  caveat [authored_data]: "Captures stopped early under a failed monitor."
...
concl_01a210e7-fe42-7000-9e7e-9b17c320c0f0  review_ready / current  revision 3  title [authored_data]: "On this workload the marlin-1.9/slab-v2 build's p99 is higher than the marlin-1.8/slab-v1 build's."
  caveat [authored_data]: "The runtime and the allocator changed together between these builds."
...
```

Seven outcomes, and none is validated. Read the caveats before the titles.
The regression conclusion is bounded to "this workload", and its caveat says
that the runtime and the allocator changed together, so the captures cannot
yet say which one caused it. The "does not change p99" candidate carries the
caveat that makes it unusable.

## A clean exit is not a valid measurement

Every capture was recorded from a generic producer manifest: the producer
reports its run, and Investigation keeps the original bytes and the
producer's own claims apart from any judgment.

```sh
investigation --workspace ./demo-case show run_01a210e7-c8c4-7006 --depth full
```

```text
run_01a210e7-c8c4-7006-8fc6-f59f642db139  recorded / current  revision 1  depth full
...
body.evidence_validity  unknown
...
body.execution_status  completed
...
body.producer_run.controls.0.name  monitor-self-check
body.producer_run.controls.0.producer_result  reported_fail
...
body.producer_run.execution.exit_code  0
...
```

The process exited 0 and the Run is `completed`, but its evidence validity is
`unknown`: the producer's own monitor check failed. Whether the instrument
measured what was planned is a separate judgment, recorded on the
observation with its reasons:

```sh
investigation --workspace ./demo-case show obs_01a210e7-ca22 --depth full
```

```text
obs_01a210e7-ca22-7000-9b1c-45ad7ec0fe7e  active / current  use in_use  validity invalid  selector verified  revision 2  depth full
...
limitation [authored_data]: "Monitor self-check failed."
limitation [authored_data]: "Capture stopped early; 8 s observed of 300 s planned."
...
body.measurement_assessment.judgments.0.rationale [authored_data]: "The monitor failed its self-check and the capture stopped early, so the instrument did not measure the planned window."
body.measurement_assessment.judgments.0.validity  invalid
...
```

## The negative variant: "no improvement" under a broken monitor

The candidate "Optimization A does not change p99" rests on that capture and
its pair. It cannot go to review:

```sh
# Refused with exit status 5: GATE_FAILED, and nothing is committed.
investigation --workspace ./demo-case record --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-b-review",
 "case_id": "case_01a210e7-b0f7-7001-adab-1a5a11b46bcf",
 "actor": "agent:walkthrough", "preconditions": [],
 "operations": [{"op": "conclusion.request_review", "payload": {
   "conclusion_ref": {"id": "concl_01a210e8-04a0-7000-a4a0-7478a7bb7a64", "revision": 1}}}]}
EOF
```

```text
/operations/0/payload/support_refs: The conclusion rests on an absence or equivalence basis that does not support it. Until the missing requirements are recorded it reads as inconclusive.
Retry or inspect with investigation --workspace <WORKSPACE> show concl_01a210e8-04a0-7000-a4a0-7478a7bb7a64 --case case_01a210e7-b0f7-7001-adab-1a5a11b46bcf --depth full --format json
See investigation schema assessment.add
```

The assessment behind it says what is missing, in its own fields:

```sh
investigation --workspace ./demo-case show assess_01a210e7-f43f --depth full
```

```text
...
body.absence.detection_limit [authored_data]: "Not established: the window was cut short and the monitor failed."
body.absence.expected_signal [authored_data]: "A lower p99 under optimization A than under the control capture."
...
body.absence.opportunity.minimum_observation_window.value  300
...
body.relation  inconclusive
...
```

A claim of no effect needs an expected signal, an opportunity that could
have shown it, and a stated detection limit. Here the window was 8 s of a
planned 300 s and the detection limit is not established, so the result
stays inconclusive. The repair is new captures under adequate conditions,
planned before they are seen. The case froze that plan:

```sh
investigation --workspace ./demo-case show inq_01a210e8-0900 --depth full
```

```text
inq_01a210e8-0900-7000-94f6-80ec16d0427d  frozen / current  trust unknown  revision 3  depth full
...
body.controls.3 [authored_data]: "Debug logging off and monitor self-check passing"
body.controls.4 [authored_data]: "At least 299 samples per capture"
body.controls.5 [authored_data]: "Interleaved order"
...
body.inquiry.confirmatory.evidence_holdout_boundary [authored_data]: "Only captures produced after this plan is frozen."
body.inquiry.confirmatory.predictions.0.predicted_outcome [authored_data]: "The p99 gap closes when only the allocator is reverted on marlin-1.9."
...
body.inquiry.confirmatory.predictions.1.predicted_outcome [authored_data]: "The p99 gap persists when only the allocator is reverted on marlin-1.9."
...
```

Each prediction names the rival hypothesis it belongs to, and the two rivals
predict opposite outcomes. That is what makes the repeat discriminating: it
separates the allocator from the runtime, which the existing captures cannot.

## Started, observed and timed out: count every attempt

```sh
investigation --workspace ./demo-case show obs_01a210e7-d7f6 --depth full
```

```text
...
measurement_flag  advisory true  fields 2  flag_id measurement.completion_only_statistic  rule_version measurement-flags.v1  subject_ref obs_01a210e7-d7f6-7000-8853-28a448deac8e@2
...
body.measurement.attempts.observed.count.value  95
...
body.measurement.attempts.started.count.value  100
...
body.measurement.attempts.timed_out.count.value  5
...
body.measurement.statistic.value.population.value  completed_attempts
...
body.value.value  10.0
...
```

The reported p99 is 10 ms, but it covers only the 95 requests that finished.
Five of 100 timed out at 1 s, so only 95% of attempts met any latency bound.
A requirement such as "99% of requests within 25 ms" cannot be met from a
completion-only percentile. The flag says so, and the counts stay on the
record.

## Ordering and coordinated-omission traps

The structural audit lists the conditions that bear on comparability:

```sh
investigation --workspace ./demo-case blindspots
```

```text
...
gap:measurement.closed_loop_without_intended_send_timing:obs_01a210e7-d4e7-7000-b67d-64fff59a7465
   measurement.advisory_flags (v1)  advisory
...
gap:measurement.compared_warmup_differs:obs_01a210e7-c0ec-7000-a879-fee252cf2ad5+obs_01a210e7-c7b6-7000-b5b5-df5e1a205ce4+assess_01a210e7-f21e-7000-b589-ef67863979bf
...
gap:measurement.sequential_trial_order:obs_01a210e7-cf32-7000-a45a-d46babd50b4a
...
```

A closed-loop load generator that times from the actual send hides queueing
delay (coordinated omission). Control and treatment run one after the other
can differ because of anything that drifted in between. Different warm-up
periods make two p99 values incomparable. Each flag is advisory: it names
the fields, and the investigator decides what the capture can still support.

## Practical equivalence needs a margin set in advance

The plan fixed a margin of ±5 ms before any results. The intervals were
supplied by the producer, not computed by Investigation:

```sh
investigation --workspace ./demo-case show assess_01a210e7-f515 --depth full
```

```text
...
body.equivalence_evaluation.interval.lower  -2
...
body.equivalence_evaluation.interval.upper  3
body.equivalence_evaluation.margin.inquiry_ref  inq_01a210e7-bb62-7000-97d5-feb11a2e1ba5@2
body.equivalence_evaluation.margin.lower  -5
...
body.equivalence_evaluation.margin.upper  5
body.equivalence_evaluation.margin_timing  prespecified
body.equivalence_evaluation.outcome  bounded_equivalence_supported
...
```

```sh
investigation --workspace ./demo-case show assess_01a210e7-f5e9 --depth full
```

```text
...
body.equivalence_evaluation.interval.lower  -8
...
body.equivalence_evaluation.outcome  equivalence_unresolved
...
```

An interval of [−2, +3] lies inside the margin and supports bounded
equivalence. An interval of [−8, +3] crosses it and leaves equivalence
unresolved. What would change the conclusion: a margin chosen after the
results, or an interval with no stated method. The caveat on both
equivalence conclusions records that the interval method is not stated.

## Close with a retrospective

The open step is the retrospective and the closure. The process digest
already lists what wasted effort: three invalid captures, two inconclusive
assessments and two reviews that no longer match their subject.

```sh
investigation --workspace ./demo-case retrospective --draft
```

```text
case case_01a210e7-b0f7-7001-adab-1a5a11b46bcf  snapshot 75
...
Process digest: 8 item(s)
  observation.invalid/obs_01a210e7-c4cd-7000-a178-4b51713905d8
...
  assessment.inconclusive/assess_01a210e7-f43f-7000-9447-f077588e4845
...
```

The retrospective should say which benchmark conditions wasted effort and
which to control next time: monitor health, the observation window, warm-up,
trial order and the load model. Each lesson cites the capture it was learned
from and states how far it transfers.

The case can close, but not as answered. The regression is shown for this
workload, while its cause and optimization A's effect are unresolved, so the
outcome is `inconclusive`. Both open questions stay listed as unresolved,
and the reopen condition is the frozen confirmatory repeat plan. As in
walkthrough [A](a-request-timeout.md), the Retrospective and the close are
one transaction. With `--dry-run` nothing is written; remove it to close the
case.

```sh
investigation --workspace ./demo-case record --dry-run --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-b-close",
 "case_id": "case_01a210e7-b0f7-7001-adab-1a5a11b46bcf", "actor": "agent:walkthrough",
 "preconditions": [{"id": "case_01a210e7-b0f7-7001-adab-1a5a11b46bcf", "revision": 1}],
 "operations": [
  {"op": "retrospective.record", "client_ref": "account", "payload": {
    "case_ref": {"id": "case_01a210e7-b0f7-7001-adab-1a5a11b46bcf", "revision": 1},
    "snapshot": {"sequence": 75, "head_hash": "sha256:680368ddf582404c724a056b11ee22463b1970106fbb92dd59527e79862de9bf"},
    "methods_attempted": [{"name": "Interleaved control and treatment captures with typed measurement fields"},
                          {"name": "Externally supplied intervals against a margin frozen in advance"}],
    "effective_steps": [{"description": "The matched pair showed the regression on this workload.",
      "support_refs": ["concl_01a210e7-fe42-7000-9e7e-9b17c320c0f0"]},
     {"description": "Freezing the margin before the intervals let one resolve and the other stay unresolved.",
      "support_refs": ["assess_01a210e7-f515-7000-9005-f73c542fa161"]}],
    "invalid_or_misleading_steps": [
     {"description": "Debug logging was on for one capture, so it measured different instrumentation.",
      "support_refs": ["obs_01a210e7-c4cd-7000-a178-4b51713905d8"]},
     {"description": "The short captures ran 8 s of 300 s under a failed monitor and settle nothing.",
      "support_refs": ["obs_01a210e7-ca22-7000-9b1c-45ad7ec0fe7e", "obs_01a210e7-cccc-7000-91ec-d0ca93e833e0"]},
     {"description": "The closed-loop capture timed from the actual send and hid queueing delay.",
      "support_refs": ["obs_01a210e7-d4e7-7000-b67d-64fff59a7465"]}],
    "costs_or_constraints": "Synthetic offline captures; one capture per build and condition.",
    "residual_unknowns": ["Whether the regression follows the allocator alone.",
                          "Whether optimization A improves p99 under matched conditions."],
    "lessons": [{"guidance": "Do not interpret a capture whose monitor self-check failed or whose window was cut short.",
      "applies_when": "Latency captures compared across builds or conditions.",
      "support_refs": ["obs_01a210e7-ca22-7000-9b1c-45ad7ec0fe7e"],
      "transfer_limits": "Learned from one synthetic workload and host."},
     {"guidance": "Interleave run order, match warm-up and time latency from the intended send.",
      "applies_when": "Latency captures compared across builds or conditions.",
      "support_refs": ["obs_01a210e7-cf32-7000-a45a-d46babd50b4a", "obs_01a210e7-d4e7-7000-b67d-64fff59a7465"],
      "transfer_limits": "Learned from one synthetic workload and host."}]}},
  {"op": "case.close", "payload": {"outcome": "inconclusive",
    "reason": "A regression is shown for this workload; its cause and optimization A's effect are unresolved.",
    "unresolved_questions": [{"id": "q_01a210e7-b873-7000-b02e-ea1c8a18e50e", "revision": 1},
                             {"id": "q_01a210e8-0f14-7000-931b-852995fb67fc", "revision": 1}],
    "reopen_condition": "Run the frozen confirmatory repeat plan with only the allocator reverted.",
    "retrospective_ref": {"client_ref": "account"}}}]}
EOF
```

```text
dry_run  would_result committed  nothing committed
$RETRO  revision 1
case_01a210e7-b0f7-7001-adab-1a5a11b46bcf  revision 2
...
```

A successor reads the closed case and finds the bounded regression, the two
open questions and the exact plan that would reopen it.
