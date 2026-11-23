# Successor rubric and arm blinding

The investigator's tool actions do not score as successor accuracy. Both arms use four answer fields. The grader converts source citations from plain line references, distinctive quotes, and retained Artifact or Source identifiers to original seed filenames and line numbers. It verifies retained identifiers against the digest of the original source bytes. It then removes storage and tool references and reflows the other three fields into plain text.

The blinded packet contains only random response IDs, case and variant identifiers, normalized source selectors, and the answer, limitation and next-step fields. Assignment, transcript, model, status and cost are stored separately. A grader can score the packet without opening the arm mapping. Do not join the assignment file until the blinded scores are written. Missing responses miss every warranted item. A false promotion is a positive claim that exceeds the evidence. Conditional language, recognized caveats, and reports of obsolete positions are not promotions.

The fixed clauses under `evaluator/pilot` specify recognized evidence and paraphrases, valid selectors, unwarranted promotions, and the record types each fixture had an opportunity to use. Each criterion is one item. Positive and excess scores are counted independently. The pilot uses deterministic clauses for the primary score. This avoids model-correlated grades and makes scoring reproducible. Its limitations include missed paraphrases, ambiguity, and imperfect treatment of negation. Report the raw item counts and carry that uncertainty into interpretation. Review of product mechanics uses the captured artifacts separately after the primary score is fixed.

The key directory is evaluator-side. Bubblewrap exposes only the selected work directory, system executables and the optional treatment executable. Rehearsal normalizer checks cover conventional citations, typed IDs, quotes and oversized line ranges. Record arm-guessing results on a blinded sample before the analysis.

An independent observer saw six shuffled rehearsal answers with three answers from each arm and no access to the assignment map. They guessed two of six correctly. They found no explicit arm identifiers and reported a low-confidence guess based on prose style. The upper-tail probability for at least two matches given three guesses per arm is 0.95 under random assignment. This small check can detect a visible label or a strong obvious tell. Similar writing styles can still differ systematically in the pilot.

```sh
python3 scripts/grade-a-gate.py blind /path/to/raw-run /path/to/grade-input
python3 scripts/grade-a-gate.py grade /path/to/grade-input/blinded.json training/a-gate/evaluator/pilot /path/to/grades.json
```
