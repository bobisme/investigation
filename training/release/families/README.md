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

That is 168 variants. With two arms and two pinned models per variant, it gives the preregistered 672 paired-arm assignments. `verify` reads the counts from the preregistration file at `214c45b` and fails if the registered set differs. The [skip-the-tool tasks](#skip-the-tool-tasks-w1402) from W14.02 are sealed in the same bundle but are not part of this count.

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

## Twins beyond the preregistered one (W14.02)

Plan §20.5 lists four kinds of twin edit: remove a decisive source, flip an operative criterion, rename an option, and add a duplicate citation. The preregistration fixes the design at one original and one twin per family. It counts 2 variants per family and 672 paired-arm assignments, and `verify` enforces both numbers. W14.02 therefore adds no extra twins to the sealed families. A third variant would change the preregistered assignment total. The §20.5 edits are already covered as follows:

- **Missing evidence.** The 42 missing-evidence twins remove the decisive source.
- **Cosmetic change.** The 42 cosmetic twins rename an option, reorder or reword lines, or add a duplicate citation.
- **Operative-criterion flip.** Every `architecture` original stages a new hard criterion after the review checkpoint (base scenario 4, "react to a changed criterion"). The warranted choice, or its conditions, must change from the reviewed provisional choice. That change since review is the warranted item `change_since_review`.
- **Expected direction.** Every twin key states `relative_to_original`. Keyed `verify` re-runs the cross-variant checks in [Authorship and independence](#authorship-and-independence). Those checks score both error directions for every family: unwarranted promotion and unnecessary abstention.
- **Separate sessions.** Each assignment is one variant in a fresh session, so no session sees both variants of a family. The W14.05 schedule must keep it that way.

## Skip-the-tool tasks (W14.02)

The preregistration says: "Separate lookup probes from this primary test." The bundle therefore also holds **24 skip-the-tool tasks**, `release-lookup-01` to `release-lookup-24`. In each task the right move is to answer directly, without building up a case:

| Kind | Tasks | What the participant does |
| --- | ---: | --- |
| `fact` | 12 | A one-shot factual lookup whose answer is stated in `source.txt`. |
| `implementation` | 12 | An ordinary small change, such as a one- to three-line code or config edit, a computed value or a filled-in field, that `source.txt` fully specifies. |

Each of the four themes (`debugging`, `measurement`, `source-research`, `architecture`) has three tasks of each kind. The theme sets what the source looks like, such as a log, a benchmark table, a catalogue entry or a decision record. That material can tempt an agent into a full investigation, but the answer is always fully determined by one read.

**Layout.** Each task is registered in `fixtures/family-registry.json` with split `sealed`, domain `lookup` and one variant, `lookup`. The variant has an objective digest and `source.txt`. It has no `update.txt`, so there is no review checkpoint. The bundle holds `sealed/<task>/lookup/objective.txt`, `sealed/<task>/lookup/source.txt`, the key `evaluator/sealed/<task>.json` and the specification `authoring/<task>.json`. The key uses the A-gate rubric format, so `grade-a-gate.py grade` reads it unchanged. Domain `lookup` is the domain the A-gate runner and analysis already report separately from the substantive families.

**Rubric.** The four warranted items are `current_disposition`, `decisive_citation`, `material_limitation` and `discriminating_followup`. A lookup has no change since review, so it has no `change_since_review` item. Each key has at least two `false_promotions` and at least two reference answers. It also has an `overclaim_answer`, which must trigger a false promotion, and an `abstention_answer`, which must miss `current_disposition`. Both error directions can therefore be scored on every task. `check-lookups` also prefixes every reference answer with "This is a direct lookup from the source and has not been independently verified." The answer must still keep `current_disposition`. This rule makes the abstention filter catch refusals but not honest labels. The [known rubric limitations](#known-rubric-limitations) apply here as well. False promotions are matched one sentence at a time. A correct answer can name a nearby distractor value, such as "the weekend sailing takes 65 minutes", in a sentence of its own with no hedge word. Some keys can then flag that answer. The authors narrowed these rules with lookaheads, but hand review of a sample should include the lookup grades.

**Grading and cost.** The tasks run in both arms and with both pinned models, under the same budgets and fresh-session rules as the families. The protocol requires the following:

- Grade the answer with the key.
- Report per-arm accuracy, missed warranted items and false promotions, with denominators.
- Report per-arm cost: metered tokens, shell calls, wall time, help, schema and onboarding bytes, and calls to the CLI. Give the paired difference from CLI to notes.
- In the CLI arm, count any committed journal event as an unnecessary use of the tool, and report the rate.

These are diagnostics, separate from the primary estimate and the release rule. If the CLI arm matches the notes arm on correctness but costs much more, that is reported as a failure (plan §20.5, Cost). The manifest's `lookup_tasks` section fixes the count, the kinds, the themes and every key digest before any run. The preregistration sets no count for these tasks.

**Authorship.** Four fresh author sessions wrote the tasks, `author-lookup-a` to `author-lookup-d`, one per theme. The W14.02 worker `twins-opus` coordinated. None of them worked on blindspot rules or onboarding. Each author saw only a short authoring brief, two worked examples outside the set and a standalone copy of the checker. The authors were told not to read anything in the repository. The manifest records them under `lookup_tasks.authorship`.

**How the seal was extended.** `seal` refuses to replace a registered sealed set, and it still does. `seal-lookups` adds tasks to the bundle once:

1. It decrypts the current bundle and checks its digest.
2. It checks every task specification and refuses any source bytes already used by a family, a development fixture or a pilot fixture.
3. It writes a new deterministic tar with the new files and re-encrypts it.
4. It appends the registrations and records the earlier bundle's digests under `bundle.previous`.

Keyed `verify` rebuilds the uncompressed tar from the non-lookup files and compares it with `bundle.previous.tar_sha256`. This proves that no family byte changed. The family entries in the manifest and the registry are not changed.

```sh
python3 scripts/seal-release-families.py check-lookups /private/authoring/dir
python3 scripts/seal-release-families.py seal-lookups /private/authoring/dir --authors authors.json
```

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
