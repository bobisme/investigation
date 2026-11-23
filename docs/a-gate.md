# A-gate pilot result

The pilot met the directional rule in ADR 0003. On six independent substantive case families, successor accuracy rose from **64.2% with Markdown notes to 76.7% when the CLI was available**. False promotions tied at **2 of 48 scored opportunities per arm**. The two treatment arms had the same prompts, local sources, deadlines and session handoff. The result concerns access to the CLI and the resulting behavior in this experiment. The stored records provide too little use of canonical evidence and review to attribute the gain to those features. [Precommitted rule](adr/0003-a-gate.md) · [Trial verification](../training/a-gate/replay.md)

## Paired outcomes

The 24 substantive pairs cover six families, two variants per family and two agent models. The table weights each family equally. The one-shot lookup is separate under the frozen protocol. The randomized assignments include incomplete sessions. A missing response misses every warranted item.

| Substantive outcome | Markdown | CLI available | Paired difference |
| --- | ---: | ---: | ---: |
| Correct successor items | 77/120 | 92/120 | +12.5 percentage points |
| Missed warranted items | 43/120 | 28/120 | −12.5 percentage points |
| Unwarranted promotions | 2/48 | 2/48 | 0 percentage points |
| All warranted items and no promotion | 5/24 | 9/24 | +4 assignments |

Resampling families within domain gave exploratory 95% percentile intervals of +2.5 to +24.2 points for accuracy and −8.3 to +8.3 points for false promotions. Estimated between-family variance of the paired accuracy difference was 0.02375 on the proportion scale and its bootstrap interval was 0.00167 to 0.03900. Six synthetic families give a wide, unstable variance estimate. Reported intervals describe these cases and the chosen resampling scheme.

| Breakdown | Markdown accuracy | CLI-available accuracy | Paired accuracy difference | False-promotion difference |
| --- | ---: | ---: | ---: | ---: |
| Debugging | 53.3% | 63.3% | +10.0 pts | 0 pts |
| Architecture choice | 75.0% | 90.0% | +15.0 pts | 0 pts |
| Qwen 3.5 9B | 75.0% | 86.7% | +11.7 pts | −4.2 pts |
| Gemma 4 26B | 53.3% | 66.7% | +13.3 pts | +4.2 pts |

The two model differences in false promotions point in opposite directions. Each investigator used one session per assignment. The two model groups and twins are observations within a family. The lookup had one family and two model pairs, with 8/8 items correct for notes and 7/8 with CLI access. Model order, per-family effects, raw counts and unweighted means are in the data files. The independent unit for an upper bound is the family. The Gemma notes subgroup had no observed false promotions across six independent families. Its exact one-sided 95% upper bound for the chance of a family with a promotion is 39.3%. In the single lookup family the corresponding bound is 95%. [Machine-readable results](../training/a-gate/results.json) · [Frozen grades](../training/a-gate/archive/blinded-grades.json)

## Burden and use

| Mean per substantive assignment, investigator plus successor | Markdown | CLI available |
| --- | ---: | ---: |
| Wall time | 51.1 s | 63.6 s |
| Shell calls | 12.7 | 18.5 |
| Metered input tokens | 5,796 | 12,509 |
| Output tokens | 4,646 | 5,177 |
| CLI help and schema bytes | 0 | 3,151 |
| Evidence bytes read or returned | 2,110 | 1,960 |
| Markdown bytes read | 6,705 | 2,934 |
| Markdown bytes written | 5,769 | 3,145 |
| Envelope preparation calls | 0 | 0.96 |

The treatment averaged 24.4% more wall time and more than twice as many metered input tokens. The local engine omitted cached prompt token counts. Reported input totals measure the counters the engine returned. Trace counts include transferred bytes and omit memory-mapped reads without an explicit byte transfer. The median first substantive CLI receipt was call 18.5 among the two investigators who produced one, and the median interval from onboarding to receipt was 16 calls among the qualifying records.

The notes arm recorded a pre-update review checkpoint in all 24 substantive assignments. The CLI-available arm recorded one in 15 of 24. The captured treatment workspaces contained three Case records across two assignments. They did not contain canonical source, evidence, comparison or review records at handoff. The source correction and criterion insertion still arrived through the paired harness. The separate worked case verifies the application's stale-review rejection. There were no recorded Hypotheses or Retrospectives to grade for distinguishing predictions or checkable findings. This sample therefore cannot estimate their quality rates. [Update exercise](../training/a-gate/review-update.md) · [Milestone A proof](milestone-a.md)

## Disposition

ADR 0003 requires a strict reduction in false promotions or a strict increase in successor accuracy. The family-weighted 12.5-point increase satisfies the second condition. The false-promotion rate tied. The pilot used six synthetic substantive families, two local models, and a deterministic rubric that can miss paraphrases and mis-handle negation. A separate observer guessed six of 16 arms from normalized responses and saw one generic tool mention. The score rules did not read arm assignment or tool metadata. The raw record supports proceeding under the pilot rule. A sealed release trial must establish whether the gain is useful at an acceptable burden. [Audit record](../training/a-gate/replay.md)
