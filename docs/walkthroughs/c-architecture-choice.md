# Walkthrough C: an architecture choice

Scenario C of plan §17.3 is an evaluative case: choose between (A) a
database-backed queue, (B) a managed queue and (C) a dedicated broker for one
known application. No option is best everywhere, so the case records options,
criteria and every option-by-criterion cell, and its decision is conditional.
This walkthrough reads the worked case that `demo` builds. See the
[index](README.md) for how the commands are run and checked.

The same binary and operation catalog handle all four walkthroughs; only
optional method cards change. The general `criteria`, `compare`,
`sensitivity` and `premortem` cards apply here. The optional
[`decision-analysis`](../../methods/decision-analysis.toml) pack adds a policy
that `decision.accept` needs a reopen trigger.

## Build the case

```sh
investigation demo ./demo-case --scenario choice
```

```text
demo  choice  <WORKSPACE>
fixture  fixtures/vault-goldens/scenario-c  replayed 44 transactions  seed 0x5eed0c17
case  case_01a20751-966d-7001-ae2d-5a18d7d5f0ce
open step  The superseding decision after the changed hard threshold awaits the retrospective and case closure; unknown comparison cells stay unknown.
...
```

## Read the handoff first

```sh
investigation --workspace ./demo-case brief --level compact
```

```text
case_01a20751-966d-7001-ae2d-5a18d7d5f0ce  active / needs_review  snapshot 44  level compact
title [authored_data]: "Queue architecture choice"
...
mode evaluate
...
outcomes  shown 3
dec_01a20751-a223-7000-b17d-bdb8f56d1dad  proposed / needs_review  revision 1  title [authored_data]: "Retain database-backed queue (do-nothing baseline)"
...
dec_01a20751-9ba9-7000-80c8-6358d78cfecd  superseded / needs_review  revision 3  title [authored_data]: "Retain database-backed queue (do-nothing baseline)"
...
```

The case is in `evaluate` mode. The first decision was accepted, and later
superseded. Neither it nor its replacement reads as "accepted and current":
both show `needs_review`. The rest of this walkthrough shows why.

## Steps 1 and 2: options, a baseline, and criteria

The three options include "retain the database-backed queue" as the explicit
do-nothing baseline. Two criteria are hard requirements and three are softer
preferences: operational cost, enqueue latency and team effort. A hard
criterion is a pass-or-fail gate, not a weight:

```sh
investigation --workspace ./demo-case show crit_01a20751-976f --depth full
```

```text
crit_01a20751-976f-7000-ac6e-6c5ec799b3dd  active / current  trust unknown  revision 2  depth full
title [authored_data]: "Recovery time after acknowledged enqueue"
...
body.change_reason [authored_data]: "Stricter synthetic recovery requirement"
body.kind  hard
...
body.material_change  true
...
body.threshold.operator  at_most
body.threshold.value.decimal  60
body.threshold.value.unit  seconds
...
```

This is revision 2. Revision 1 allowed 300 seconds; after the first decision
was accepted, the threshold changed to 60 seconds. That change is the
adversarial variant of this scenario.

## Step 3: every cell, and unknown stays unknown

The decision record export includes the comparison matrix. It computes no
score and no ranking:

```sh
investigation --workspace ./demo-case export --kind decision-record --audience internal
```

```text
exported  decision-record  complete  exports/decision-record-case_01a20751-966d-7001-ae2d-5a18d7d5f0ce-44-fe655fdab412
...
```

```sh
cat ./demo-case/exports/decision-record-case_01a20751-966d-7001-ae2d-5a18d7d5f0ce-44-fe655fdab412/decision-record.md
```

```text
# Decision record: Queue architecture choice
...
### Comparison matrix

Each cell is what is recorded for one option against one criterion at this snapshot. This matrix computes no overall score and no ranking; an unknown or missing cell is not a pass.

| Option | Recovery time after acknowledged enqueue (hard) | No acknowledged job is lost (hard) | Operational cost estimate (soft preference) | Enqueue latency \(small synthetic benchmark\) (soft preference) | Team implementation effort (soft preference) |
|---|---|---|---|---|---|
| Retain database\-backed queue \(do\-nothing baseline\) | needs review: comparison is out of date: outcome `meets`, `110` `seconds`, 1 evidence (`cmp_01a20751-98cb-7000-aecf-294d4c561954`) | unknown: outcome `unknown`, because "Independent verification of loss is missing" ...
| Adopt managed queue | needs review: comparison is out of date: outcome `unknown`, because "managed queue lacks the required recovery measurement" ...
| Run dedicated broker | needs review: comparison is out of date: outcome `unknown`, because "dedicated broker lacks the required recovery measurement" ...
...
```

Most cells are `unknown`, each with its reason. A cell nobody measured is
not a pass, and the matrix does not fill it in. The baseline's recovery cell
says `meets` at 110 seconds, which was true against 300 seconds and is false
against 60. The matrix marks it out of date instead of keeping the old
answer.

## Step 4: what could reverse the choice

The original decision records the assumption it rests on and the condition
that would reverse it:

```sh
investigation --workspace ./demo-case show dec_01a20751-9ba9 --depth full
```

```text
dec_01a20751-9ba9-7000-80c8-6358d78cfecd  superseded / needs_review  trust untrusted_external  revision 3  depth full
...
freshness_reason  cause crit_01a20751-976f-7000-ac6e-6c5ec799b3dd@2  freshness needs_review  path 1  trigger.kind material_revision
...
body.hard_conditions.0.reason [authored_data]: "Independently verify the reported p95 and failure model"
...
body.reopen_trigger [authored_data]: "Retest recovery if the threshold changes or traffic exceeds 2500000 jobs/day"
...
body.reversal_conditions.0.condition [authored_data]: "A peak-traffic cost measurement favors the managed queue"
...
body.superseded_by  dec_01a20751-a223-7000-b17d-bdb8f56d1dad@1
body.tradeoffs.0 [authored_data]: "The benchmark suggests higher enqueue latency; costs are estimates"
...
```

The decision was conditional from the start: it names hard conditions still
to verify, a reopen trigger, and a reversal condition on traffic cost. Its
own reopen trigger fired when the threshold changed. The freshness reason
names the cause: revision 2 of the recovery criterion.

## The adversarial variant: the hard threshold changed after the decision

The replacement decision cannot be accepted with the review that supported
the original:

```sh
# Refused with exit status 5: the review does not cover the current basis.
investigation --workspace ./demo-case record --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-c-accept",
 "case_id": "case_01a20751-966d-7001-ae2d-5a18d7d5f0ce",
 "actor": "agent:walkthrough", "preconditions": [],
 "operations": [{"op": "decision.accept", "payload": {
   "decision_ref": {"id": "dec_01a20751-a223-7000-b17d-bdb8f56d1dad", "revision": 1},
   "review_ref": {"id": "rev_01a20751-9c08-7001-ad97-3da4beac5791", "revision": 1}}}]}
EOF
```

```text
/operations/0/payload/review_ref: Obtain a current review against this claim and its scoped membership.
Retry or inspect with investigation --workspace <WORKSPACE> show dec_01a20751-a223-7000-b17d-bdb8f56d1dad --case case_01a20751-966d-7001-ae2d-5a18d7d5f0ce --depth full --format json
See investigation schema decision.accept
```

Nothing is committed. The repair starts with the out-of-date cells: each one
is revised against the current criterion revision, with the same evidence.
Here the baseline's 110 seconds now fails the 60-second limit:

```sh
investigation --workspace ./demo-case record --dry-run --input - <<'EOF'
{"schema_version": "inv.command.v1", "command_id": "walkthrough-c-revise",
 "case_id": "case_01a20751-966d-7001-ae2d-5a18d7d5f0ce", "actor": "agent:walkthrough",
 "preconditions": [{"id": "crit_01a20751-976f-7000-ac6e-6c5ec799b3dd", "revision": 2}],
 "operations": [{"op": "comparison.revise", "payload": {
   "comparison_ref": {"id": "cmp_01a20751-98cb-7000-aecf-294d4c561954", "revision": 1},
   "reason": "The recovery threshold changed from 300 to 60 seconds.",
   "replacement": {
     "option_ref": {"id": "opt_01a20751-96ac-7000-848d-b31bbda8062f", "revision": 2},
     "criterion_ref": {"id": "crit_01a20751-976f-7000-ac6e-6c5ec799b3dd", "revision": 2},
     "evidence_refs": ["obs_01a20751-98a7-7000-975b-065951c6309a"],
     "value": {"decimal": "110", "unit": "seconds"},
     "method": "synthetic recovery trial; p95 reported by source",
     "outcome": "fails",
     "rationale": "110 seconds is above the 60-second threshold",
     "uncertainty": "Only 30 synthetic trials under the stated failure model",
     "assumption_refs": []}}}]}
EOF
```

```text
dry_run  would_result committed  nothing committed
cmp_01a20751-98cb-7000-aecf-294d4c561954  revision 2
...
```

The source evidence is unchanged; only its reading against the new
threshold changes. With the baseline failing a hard requirement, the
proposed decision to retain it can no longer stand. The next steps are a
fresh review of whatever decision the revised matrix supports, and then
`decision.accept` with that review. Remove `--dry-run` to commit the
revision.

## Step 5: audit, challenge, retrospective

```sh
investigation --workspace ./demo-case blindspots
```

```text
...
blindspot:criterion.hard_unmeasured:crit_01a20751-976f-7000-ac6e-6c5ec799b3dd
   criterion.hard_unmeasured (v2)  material
   A hard requirement has no evidence-backed disposition for at least one applicable option.
   subjects: crit_01a20751-976f-7000-ac6e-6c5ec799b3dd@2
...
blindspot:criterion.hard_unmeasured:crit_01a20751-9798-7000-aae4-926efd533c7d
...
```

Both hard requirements lack an evidence-backed result for at least one
option: no one has measured whether an acknowledged job can be lost. That
gap could change the answer, so it belongs in a challenge Review and an open
Question, not in an invented replacement conclusion. The case then closes
with an attributed Retrospective in the same transaction, as walkthrough
[A](a-request-timeout.md) shows.
