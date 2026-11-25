# Walkthrough A: an intermittent request timeout

Scenario A of plan §17.1 is an explanatory case: a request sometimes hangs
after a deployment. Was a database connection held across an asynchronous
wait, or was the scheduler responsible? This walkthrough reads the worked case
that `demo` builds and explains each step of the method: why it is taken, what
to look at, and what would change the conclusion. See the
[index](README.md) for how the commands are run and checked.

The same binary and operation catalog handle all four walkthroughs; only
optional method cards change. The general cards named here are built in. The
optional [`debugging`](../../methods/debugging.toml) pack specializes them for
this kind of case.

## Build the case

```sh
investigation demo ./demo-case --scenario debug
```

```text
demo  debug  <WORKSPACE>
fixture  fixtures/vault-goldens/scenario-a  replayed 28 transactions  seed 0x5eed0a17
case  case_01a20751-8e54-7001-b14b-62fb37d7b21b
open step  The validated root cause awaits its attributed retrospective and case closure.
...
```

The demo replays 28 committed transactions with their real IDs. The same
command always gives the same IDs, so you can paste the IDs below.

## Read the handoff first

A fresh agent starts from the bounded handoff, not from raw captures.

```sh
investigation --workspace ./demo-case brief --level compact
```

```text
case_01a20751-8e54-7001-b14b-62fb37d7b21b  active / current  snapshot 28  level compact
title [authored_data]: "Why does the synthetic lookup stall?"
mode explain
caveat [authored_data]: "The scheduler may contribute; the incompatible-build trial cannot isolate its effect."
contradiction  known  hyp_01a20751-8eb8-7000-b6b4-f1e1b7b3632f@1
...
outcomes  shown 2
concl_01a20751-9308-7000-a5ec-54194a8bad75  validated / current  revision 3  title [authored_data]: "Holding the database connection across the asynchronous wait explains the overshoot in these recorded attempts."
...
concl_01a20751-91ae-7000-a632-b287464ae6d2  candidate / current  revision 1  title [authored_data]: "Holding the database connection across the asynchronous wait explains the overshoot in these recorded attempts."
...
```

Look at three things. Two conclusions have the same statement, but one is
`validated` and one is only a `candidate`: the statement is not what makes a
conclusion count; its evidence is. Every conclusion carries its caveat. The
rival explanation is listed as a known contradiction, not deleted.

## Steps 1 and 2: a question and rival hypotheses

The case records its question before any answer, and two explanations that
predict different things.

```sh
investigation --workspace ./demo-case show hyp_01a20751-8e99 --depth full
```

```text
hyp_01a20751-8e99-7000-bc0f-1ebb8d626271  proposed / current  trust unknown  revision 1  depth full
...
body.possible_falsifiers.0 [authored_data]: "The failure persists with the supposed factor isolated."
body.question_ref  q_01a20751-8e7a-7000-a598-62f75a476198@1
body.statement [authored_data]: "A checked-out connection is held during the asynchronous wait."
...
```

The rival, `hyp_01a20751-8eb8`, says that scheduler delay alone explains the
overshoot. Each hypothesis states what would falsify it. Without a live rival,
any trace that shows a long wait looks like support. The `alternatives` card
says when to add one.

## Step 3: freeze the plan before the results

```sh
investigation --workspace ./demo-case show inq_01a20751-8ed5 --depth full
```

```text
inq_01a20751-8ed5-7000-8518-d7d5d3bdf8cf  frozen / current  trust unknown  revision 2  depth full
...
body.controls.0 [authored_data]: "Same workload, route, and dataset."
body.desired_observations.0 [authored_data]: "Connection hold time and compatibility of captured attempts."
body.inquiry.mode  exploratory
body.lifecycle  frozen
...
```

The inquiry was frozen (revision 2, `plan_digest` recorded) before the
confirming captures were recorded. A plan that is written after the results
can be fitted to them. The card for this step says the same:

```sh
investigation --workspace ./demo-case guide discriminate
```

```text
discriminate  Choose a discriminating test
...
When: The next planned observation would come out the same whichever rival is true.
...
Mistake: Gathering more evidence that every rival already predicts.
...
```

## Step 4: a control, a change, a reversal, and an unusable trace

The case holds five Runs. Three were made on the same build: the slow
baseline (`run_01a20751-903e`), the toggle that releases the connection before
the wait (`run_01a20751-9088`), and the reversal of that toggle
(`run_01a20751-9118`). One trace has a clock that cannot be aligned:

```sh
investigation --workspace ./demo-case show run_01a20751-9161 --depth full
```

```text
run_01a20751-9161-7000-916f-42ca3f722ced  recorded / current  revision 1  depth full
...
body.environment [authored_data]: "Unaligned wall clock"
...
body.evidence_validity  invalid
...
```

The Run is kept and marked `invalid`. It is not negative evidence for either
hypothesis: a trace whose events cannot be ordered says nothing about which
came first.

## The adversarial variant: a refutation from a different build

The fifth Run is the trap. It seems to refute the scheduler explanation, but
it was captured on another build:

```sh
investigation --workspace ./demo-case show run_01a20751-90d1 --depth full
```

```text
run_01a20751-90d1-7000-93b9-dcaf6ace49bb  recorded / current  revision 1  depth full
...
body.environment [authored_data]: "build-7c2b and bypassed callback queue"
...
body.variables_changed.0  bypass_scheduler
...
```

The candidate conclusion uses that Run as its treatment. The structural audit
reports it:

```sh
investigation --workspace ./demo-case blindspots
```

```text
...
blindspot:comparability.environment_differs:concl_01a20751-91ae-7000-a632-b287464ae6d2
   comparability.environment_differs (v1)  material
   Evidence used together was recorded under a different build or environment.
   subjects: concl_01a20751-91ae-7000-a632-b287464ae6d2@1
...
```

The audit is a list of structural gaps, not a certificate: its last line says
that an empty result would not prove completeness. To move the candidate to
review, a request must pass the review gate. It does not:

```sh
# Refused with exit status 5: GATE_FAILED, and nothing is committed.
investigation --workspace ./demo-case record --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-a-review",
 "case_id": "case_01a20751-8e54-7001-b14b-62fb37d7b21b",
 "actor": "agent:walkthrough", "preconditions": [],
 "operations": [{"op": "conclusion.request_review", "payload": {
   "conclusion_ref": {"id": "concl_01a20751-91ae-7000-a632-b287464ae6d2", "revision": 1}}}]}
EOF
```

```text
/operations/0/payload/causal_test: Record a compatible, matching build and environment for the baseline, treatment, and reversal.
Retry or inspect with investigation --workspace <WORKSPACE> show concl_01a20751-91ae-7000-a632-b287464ae6d2 --case case_01a20751-8e54-7001-b14b-62fb37d7b21b --depth full --format json
See investigation schema conclusion.request_review
```

The refusal names the field and the repair. The test suite also checks that
the journal is byte-identical after it. The repair is the validated
conclusion: its causal test cites only same-build Runs.

```sh
investigation --workspace ./demo-case explain concl_01a20751-9308
```

```text
...
  obs_01a20751-9065-7000-b085-98aebf3ce4b0 r1 via causal_test.baseline (cites)  [active / active / current]  title [authored_data]: "obs obs_01a20751-9065-7000-b085-98aebf3ce4b0"
...
  obs_01a20751-90af-7000-80eb-94ad272fab4a r1 via causal_test.treatment (cites)  [active / active / current]  title [authored_data]: "obs obs_01a20751-90af-7000-80eb-94ad272fab4a"
...
  obs_01a20751-913f-7000-8782-30c0a4768812 r1 via causal_test.reversal (cites)  [active / active / current]  title [authored_data]: "obs obs_01a20751-913f-7000-8782-30c0a4768812"
...
```

What would change the conclusion: a same-build trial that isolates the
scheduler. That is why the caveat stays on the validated conclusion, and why
the scheduler question stays open.

## Steps 5 and 6: a bounded result and the blindspot audit

The validated conclusion is bounded to "these recorded attempts" and to one
build family, workload and route. The audit above also reports two gaps on
it: `empirical.confirmatory_basis_incomplete` (no sample count or observation
window is recorded) and `conclusion.causal_claim_without_rival_account`. The
next step is a blindspot Review that gives each reported gap a disposition and
names the scheduler gap that no check names. The `blindspot` card describes
it; `investigation schema review.submit` gives its payload.

## Step 7: retrospective and closure, in one transaction

The case is material, so it cannot close without a current Retrospective.
Start from the process digest, which lists what the journal recorded:

```sh
investigation --workspace ./demo-case retrospective --draft
```

```text
case case_01a20751-8e54-7001-b14b-62fb37d7b21b  snapshot 28
This digest covers recorded work only. Effort, dead ends, rejected writes and decisions that were never recorded in this case are absent, so it is not a complete account of the process.

Process digest: 1 item(s)
  run.invalid/run_01a20751-9161-7000-916f-42ca3f722ced
    The Run's evidence is recorded as invalid.  cite run_01a20751-9161-7000-916f-42ca3f722ced
...
```

A close without a retrospective is refused, with the command that drafts one:

```sh
# Refused with exit status 5, and nothing is committed.
investigation --workspace ./demo-case record --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-a-close",
 "case_id": "case_01a20751-8e54-7001-b14b-62fb37d7b21b", "actor": "agent:walkthrough",
 "preconditions": [{"id": "case_01a20751-8e54-7001-b14b-62fb37d7b21b", "revision": 1}],
 "operations": [{"op": "case.close", "payload": {"outcome": "answered",
   "reason": "The compatible toggle and reversal isolate the scoped mechanism.",
   "unresolved_questions": [{"id": "q_01a20751-8e7a-7000-a598-62f75a476198", "revision": 1}],
   "reopen_condition": "A compatible scheduler-only trial changes the result."}}]}
EOF
```

```text
/operations/0/payload/retrospective_ref: A material case needs retrospective_ref: a current Retrospective's ID, or the client_ref of a retrospective.record earlier in this transaction. The case has 2 material revision(s), listed in actual; the repair drafts a Retrospective.
Retry or inspect with investigation --workspace <WORKSPACE> --case case_01a20751-8e54-7001-b14b-62fb37d7b21b retrospective --draft --format json
See investigation schema retrospective.record
```

The repair records the Retrospective and the close together. Each step it
describes cites the Runs that support it, including the invalid trace. With
`--dry-run` the transaction is checked like a commit and nothing is written;
remove `--dry-run` to close the case.

```sh
investigation --workspace ./demo-case record --dry-run --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-a-close",
 "case_id": "case_01a20751-8e54-7001-b14b-62fb37d7b21b", "actor": "agent:walkthrough",
 "preconditions": [{"id": "case_01a20751-8e54-7001-b14b-62fb37d7b21b", "revision": 1}],
 "operations": [
  {"op": "retrospective.record", "client_ref": "methods", "payload": {
    "case_ref": {"id": "case_01a20751-8e54-7001-b14b-62fb37d7b21b", "revision": 1},
    "snapshot": {"sequence": 28, "head_hash": "sha256:ed3ac9a18b2cba56ddb31be45c1b7bef12f2f77a2b8d47079f1af8d61a03dc71"},
    "methods_attempted": [{"name": "Compare a slow/control pair and a bounded toggle and reversal"}],
    "effective_steps": [{"description": "The same-build toggle and reversal separated the connection hold from the rival.",
      "support_refs": ["run_01a20751-903e-7000-94ec-440881a456ba", "run_01a20751-9088-7000-941b-479396aa8579", "run_01a20751-9118-7000-b17e-7aebde1f76d2"]}],
    "invalid_or_misleading_steps": [{"description": "The trace with an unmapped clock could not order events.",
      "support_refs": ["run_01a20751-9161-7000-916f-42ca3f722ced"]}],
    "costs_or_constraints": "Offline synthetic captures only.",
    "residual_unknowns": ["The scheduler's own contribution under a compatible build."],
    "lessons": [], "no_reusable_lesson_reason": "The result is scoped to one synthetic workload."}},
  {"op": "case.close", "payload": {"outcome": "answered",
    "reason": "The compatible toggle and reversal isolate the scoped mechanism.",
    "unresolved_questions": [{"id": "q_01a20751-8e7a-7000-a598-62f75a476198", "revision": 1}],
    "reopen_condition": "A compatible scheduler-only trial changes the result.",
    "retrospective_ref": {"client_ref": "methods"}}}]}
EOF
```

```text
dry_run  would_result committed  nothing committed
$RETRO  revision 1
case_01a20751-8e54-7001-b14b-62fb37d7b21b  revision 2
...
```

The closure keeps the scheduler question open and states the trial that would
reopen the case. A successor reads `brief` and finds the bounded answer, its
caveat and the reopen trigger, with exact evidence selectors.
