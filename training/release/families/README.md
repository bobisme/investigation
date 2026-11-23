# Sealed release-trial case families (W14.01)

These are the held-out case families for the sealed release trial. Only results on these families can support a release claim. The checkout holds commitments to the families, not their content: the source bytes, objectives and grading keys are in an encrypted bundle, and the passphrase is kept outside the repository. [Preregistration](../../../docs/release-trial.md) · [Family registry](../../../fixtures/family-registry.json) · [Seal tool](../../../scripts/seal-release-families.py)

## Counts

The preregistration at `214c45b` says: "Enroll **84 independent families**, 21 each for intermittent debugging, p99 regression measurement, source research and architecture choice. Each family has an original variant and a prespecified missing-evidence or cosmetic twin. Keep those twin types as evenly distributed within each domain as the odd count allows."

| Domain (base scenario) | Families | Missing-evidence twins | Cosmetic twins |
| --- | ---: | ---: | ---: |
| `debugging` (1: intermittent bug, good control, invalid confounded trace) | 21 | 11 | 10 |
| `measurement` (2: p99 regression, meaningful measurement or broken instrument) | 21 | 10 | 11 |
| `source-research` (3: dependent "independent" citations and a correction) | 21 | 11 | 10 |
| `architecture` (4: hard and soft constraints, conditional choice, changed criterion) | 21 | 10 | 11 |
| Total | 84 | 42 | 42 |

That is 168 variants. With two arms and two pinned models per variant, it gives the preregistered 672 paired-arm assignments. `verify` reads the counts from the preregistration file at `214c45b` and fails if the registered set differs. The skip-the-tool lookup tasks belong to W14.02 and are not part of this count.

## What is in the checkout

- `fixtures/family-registry.json` registers each family with split `sealed` under an opaque ID, for example `release-measurement-07`. Each entry records the domain, the key locator `sealed/<family>.json` and two variants, `original` and its twin. For each variant it records the SHA-256 digests of the objective, `source.txt` and `update.txt`. A sealed objective is registered by digest only. `scripts/check-fixture-families.py` rejects a plain-text sealed objective and any `fixtures/sealed/` directory in the checkout.
- `manifest.json` gives the preregistered counts and the authorship declaration. For each family it gives the twin type and the digest of its grading key. It also records the digests of the plaintext bundle and the ciphertext, and the exact encryption command.
- `sealed.tar.gz.enc` is the bundle. It is a deterministic PAX tar that is gzip-compressed and then encrypted with `openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -md sha256`.

## What is in the sealed bundle

| Path | Content | Who may see it during the trial |
| --- | --- | --- |
| `sealed/<family>/<variant>/objective.txt` | The objective, given to the participant in the prompt | The participant's prompt only |
| `sealed/<family>/<variant>/source.txt` | Initial seed source | The participant, through `stage_source` |
| `sealed/<family>/<variant>/update.txt` | Evidence that the harness stages after the review checkpoint | The participant, after review |
| `evaluator/sealed/<family>.json` | Grading key | The evaluator, after blinding |
| `authoring/<family>.json` | The author's specification that the other files were generated from | Nobody until after the trial |

Each grading key uses the A-gate rubric format that `scripts/grade-a-gate.py grade` reads. Every variant has the same five warranted items: `current_disposition`, `change_since_review`, `decisive_citation`, `material_limitation` and `discriminating_followup`. Each variant also has at least two `false_promotions`. A key also records the following:

- `handoff`: what the review checkpoint sees and what `update.txt` changes in each variant.
- `expected`: the warranted answer. A twin also has `relative_to_original`, the expected direction of change. A missing-evidence twin changes the answer. A cosmetic twin does not.
- `rivals`: real alternative explanations or options, each with a prediction that distinguishes it. W14.06 uses these to grade strawman rivals.
- `reference_answers`: two or more differently phrased answers that must score every warranted item and no false promotion.
- `overclaim_answer`: an answer that must trigger a false promotion.
- `abstention_answer` and `hedged_answer`: unwarranted hedges that must miss `current_disposition`. The second names the right answer but calls it unproven. Variants where an answer is warranted have both.

The format is arm-agnostic. A family contains the objective, two seed files and a rubric over the four normalized answer fields and seed-document selectors. It does not name a tool, a record type or an arm, so the notes arm, the CLI arm and the planned help-only arm (W17.03) all use the same families.

## Authorship and independence

Fresh agent sessions created for W14.01 wrote the families: a coordinating worker (`families-opus`) and eight author sessions, two per domain. None of them worked on blindspot rules or onboarding. The authors were told not to read the repository's onboarding text, help, docs or blindspot rules. Each author saw only a domain brief, a worked example that is not in the set, and the authoring checker. The situations are synthetic and use invented names. They share no situation with the development or pilot families listed in the registry, and no source bytes with any registered development or pilot file. `seal` and keyed `verify` check the bytes.

Before sealing, `check-authoring` grades every reference, overclaim, abstention and hedged answer with the A-gate grader. It also runs these cross-variant checks:

- For a missing-evidence twin, each of the original's reference answers must count as a false promotion. Each of the twin's own reference answers must miss the original's `current_disposition`, so an unnecessary abstention scores as a missed warranted answer.
- For a cosmetic twin, each of the original's reference answers must still meet every non-citation warranted item.
- Each warranted reference answer must keep `current_disposition` when a sentence that narrates the superseded review state is prepended ("At review the leading candidate looked likely but was pending, and one input was confounded and unverified."). Abstention filters therefore target the current disposition, not every hedge word.

Together these checks show that both error directions, unwarranted promotion and unnecessary abstention, can be scored for every family.

### Known rubric limitations

The rubrics are deterministic regular expressions, as in the A-gate pilot. They can miss unusual paraphrases. They can also mark a correct answer wrong if it uses a current-disposition abstention phrase for something else, for example "the earlier figure was not confirmed". The grader excuses a sentence from false-promotion matching when it contains a hedge word. That list includes `fail` and `fails`, so a flat overclaim phrased with those words is not counted. Report raw item counts and carry this uncertainty into the analysis. Review a sample of graded answers by hand before interpreting the result.

## Verifying the seal

You can verify without the passphrase:

```sh
python3 scripts/seal-release-families.py verify --public
git log --format='%H %cI' -- training/release/families/manifest.json fixtures/family-registry.json
```

This checks the counts against the preregistration, agreement between registry and manifest, opaque IDs, one twin per family, shared initial bytes within a family, distinct sources across families, and the ciphertext digest. It also checks that no plaintext sealed path exists in the checkout. The commit time of `manifest.json` shows when the key digests were committed. W14.08 compares it with the first scored run.

An evaluator with the passphrase can open the seal:

```sh
python3 scripts/seal-release-families.py verify --key-file /path/outside/repo/release-families.key
python3 scripts/seal-release-families.py open /private/evaluator/dir --key-file /path/outside/repo/release-families.key
```

Keyed `verify` decrypts the bundle into a temporary `0700` directory and checks the plaintext bundle digest. It checks every objective, source, update and key digest against the registry and manifest. It checks that each key was generated from its authored specification. It runs `check-fixture-families` against the opened sources and repeats every rubric self-test. The temporary directory is then deleted. `open` refuses a destination inside the repository and writes files in mode `0400`. Without `--key-file`, the tool reads the passphrase file from `$INVESTIGATION_SEALED_KEY` or from `~/.local/share/investigation-evaluator/release-families.key`. It refuses a file inside the repository or one readable by group or others.

## Rules for the trial

- Open the seal only on the evaluator side, outside every participant mount. The W14.05 harness reads objectives and stages `source.txt` and `update.txt` from the opened directory with `check-fixture-families.stage_source`. It passes `<opened>/evaluator/sealed` to `grade-a-gate.py grade` after blinding.
- Builders see the sealed content only after the trial. If you tune on failures, use fresh held-out families with new IDs and a new manifest. `seal` refuses to replace a registered sealed set.
- The passphrase file is the only way to recover the content. Keep a backup outside the repository.
