# Gate-content grading

A gate that asks for a rival hypothesis can be satisfied by a rival nobody believes. A gate that asks for a retrospective can be satisfied by a platitude. The release trial therefore grades what agents put through the gates, not only that they passed. Plan §20.5: "Graders judge whether recorded rivals were real alternatives with a distinguishing prediction and whether retrospectives state anything checkable. Report the strawman rate."

The grader is `scripts/grade-gate-content.py` (stdlib Python, rubric version `gate-content.v1`). It reads the case journal of an `inv`-arm run after `investigation check` has verified it, the same way `scripts/trial-tasks.py` does. It never reads a transcript or prose. The thresholds are constants at the top of the script, and `grade-gate-content.py rubric` prints them. They are committed before the sealed trial. A change to a rule or a threshold needs a new `RUBRIC_VERSION`, and results graded under two versions are never pooled.

```sh
python3 scripts/grade-gate-content.py grade --bin target/debug/investigation --workspace RUN_DIR --key KEY --agent-family FAMILY > graded.json
python3 scripts/grade-gate-content.py summarize graded-*.json
python3 scripts/grade-gate-content.py sample graded-*.json --seed 1 --size 20
python3 scripts/grade-gate-content.py rehearse --bin target/debug/investigation
```

`--key` is a key file, or a development key name from `training/gate-content/keys/`. `--agent-family` is the label the trial harness gives the agent family (the pinned model). `--after` takes a JSON file of per-case head sequences from setup, so that records written before the participant started are not graded.

## Metric 1: strawman-rival rate

**Unit.** One recorded rival: the current revision of a Hypothesis or an Option written in the graded journal, not retracted, retired, withdrawn or superseded. A hypothesis that was recorded and then rejected within scope stays in the denominator, because it was a rival that someone had to rule out.

**Denominator.** The number of recorded rivals across the runs graded. A run with no recorded rival adds nothing. Its rate is undefined (reported as `null`, never as 0), and the summary counts such runs in `runs_with_no_recorded_rival`.

**Strawman.** A recorded rival is a strawman when at least one of these holds:

1. `no_key_rival`: it does not match a real rival of the key. The key (`rivals`, a list of `{explanation, distinguishing_prediction}`) is written by the family's author, before any run, as the real alternatives for this evidence. A recorded rival matches a key rival when the two share at least 3 content tokens, and those are at least 50% of the shorter side's content tokens (casefolded, stop words dropped, plural `s` removed).
2. `duplicate_of_key_rival`: it matches a key rival that an earlier recorded rival already matched. A paraphrase is not a second alternative.
3. `no_distinguishing_prediction`: the journal records no prediction that tells it apart from the favoured account. For a Hypothesis this means one of:
   - a live confirmatory InquiryPlan predicts an outcome for it, the plan predicts for at least two hypotheses, the outcome has at least 4 content tokens, it does not repeat the hypothesis's own statement (overlap of at most 80%), and no other hypothesis in that plan has the same predicted outcome; or
   - an Assessment records `more_expected` or `less_expected` for it in `rival_expectations`, with a rationale of at least 4 content tokens. `equally_expected` and `not_assessed` do not count.

   For an Option it means a Decision whose `reversal_conditions` name the option with a condition of at least 4 content tokens that no other option shares. A hypothesis's `possible_falsifiers` do not count, because the operation requires the field.

**Rate.** `strawman rivals / recorded rivals`, with the counts, the by-reason counts, and a Wilson 95% interval. A unit with two reasons is counted once in the rate and once in each reason. Other counts in each graded result:

- `key_rivals_covered / key_rivals`: how many real rivals the run recorded without being a strawman. A run that records one good rival and omits the others has a low strawman rate and low coverage. Report both.
- `prediction_agrees_with_key`: recorded predictions that share at least 2 tokens (at least 40% of the shorter side) with the key's `distinguishing_prediction`. It is informational and does not decide the strawman flag, because an agent may name a different valid test.

**Keys.** The sealed families of W14.01 carry `rivals` in the same shape in the grading key (`evaluator/sealed/<family>.json`), so `--key` accepts an opened sealed key without a change. Development keys for the three trial-task families are in `training/gate-content/keys/`. They are the calibration set. The grader is never pointed at the sealed bundle until the trial's grading step.

## Metric 2: checkable-retrospective rate

**Unit.** One proposed lesson of a current, unretracted Retrospective in the journal.

**Denominator.** The number of lessons across the runs graded. A retrospective that has no lesson and states `no_reusable_lesson_reason` adds nothing to the denominator and is counted in `retrospectives_without_lesson_and_with_reason`, so an honest "nothing to transfer" is not punished. A run with no lesson has an undefined rate (`runs_with_no_lesson`).

**Checkable.** A lesson is checkable when all five clauses hold:

1. `trigger_is_conditional`: `applies_when` names a condition (when, if, whenever, once, after, before, while, where, unless, on).
2. `trigger_is_specific`: `applies_when` has at least 4 content tokens, at least 2 of them outside a generic list (case, problem, evidence, data, result and similar), and it does not open with or contain "always", "in general", "any/every/all investigation(s), case(s), problem(s), task(s)".
3. `action_is_concrete`: `guidance` has at least 4 content tokens, uses an action verb from a fixed list (check, compare, isolate, measure, request, repeat and so on) in its first four words, and matches no vague-advice phrase ("be careful", "pay attention", "keep in mind", "use good judgement", and so on).
4. `cites_case_events`: `support_refs` is not empty and every reference resolves to a record of the same case in the journal. The store already requires a same-case citation; the grader repeats the check so a journal from any source is graded alike.
5. `no_praise`: neither field contains praise or success language (excellent, went well, worked very well, smoothly, flawless and similar).

A later reader can test the trigger, and knows what to do when it is true. The checks do not judge whether the lesson is true. That needs the outcome check of `lesson.admit`, which is not part of this grade.

**Rate.** `checkable lessons / lessons`, with counts, `failed_by_check` and a Wilson interval. Each graded result also reports `invalid_steps_without_resolving_support / invalid_step_denominator`: the share of recorded invalid or misleading steps that cite nothing in the journal. This is the nearest structural test for invented failures. It is reported beside the rate and is not part of it.

## Blindspot reviews

The bone's third rubric, whether the recorded gaps of a blindspot review are specific to the case, is not implemented here. A deterministic grade of specificity needs a reference list of case gaps per family, which the W14.01 keys do not carry. It is left as a follow-up.

## Report format

`summarize` prints, for each agent family and overall: rival and lesson counts with denominators and Wilson intervals, runs with an undefined rate, and key-rival coverage. The trial report quotes these counts as they are. A family's runs share a model and a prompt, so they are correlated. The interval treats units as independent and so understates the uncertainty. If a model grader is later added to read rivals or lessons, its grades are correlated with each other and with the agents' own errors. Report them beside the deterministic grade, not as independent confirmation.

## Limits

- The matcher compares words, not meaning. A strawman that borrows the key's vocabulary can match a real rival, and a real rival in unusual words can miss. Hand review a sample (`sample` draws a deterministic sample of units) and report how many grades changed.
- A recorded prediction is checked for presence, distinctness and substance, not for being a true consequence of the rival. `prediction_agrees_with_key` is a partial guard.
- A hypothesis recorded only in free text, in a note or in the answer, is not a recorded rival. The metric grades gate content that went through the journal.
- Findings may reach the method catalog only through an ADR (plan §20.5).

## Rehearsal

`rehearse` writes runs through the public binary in temporary workspaces: a good run and a strawman run for each of the three development families, then changes one clause at a time. `crates/investigation-cli/tests/gate_content.rs` runs it as part of `just check`. It asserts:

| Run | Strawman rivals | Checkable lessons |
| --- | ---: | ---: |
| Good: three key rivals, distinct plan predictions, two conditional and cited lessons | 0 of 3 | 2 of 2 |
| Strawman: three filler rivals with vague predictions, three platitude lessons | 3 of 3 | 0 of 3 |

Single-clause variants of the good run show that each clause can fail by itself: three real rivals with one shared prediction (3 of 3 `no_distinguishing_prediction`), no plan at all (3 of 3), one paraphrase padding a real rival (1 of 4, a duplicate), two real rivals and one filler (1 of 3), and a lesson that fails only the trigger, only the action or only the praise clause. Two Options graded through a Decision's reversal conditions, and an empty run with an undefined rate, are also checked. The development fixtures calibrate the thresholds only. No sealed content was opened.
