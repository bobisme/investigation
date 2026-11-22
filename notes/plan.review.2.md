# Review 2: Investigation implementation plan (`notes/plan.md`, revision 3)

**Reviewed:** `notes/plan.md`, revision 3 (2,265 lines), read in full on 2026-11-21.
**Relation to `plan.review.1.md`:** this is a second, independent review. I formed my findings before skimming review 1's proposals, then cut what it already covers. This document concentrates on what review 1 missed, extends three of its changes, and disagrees with it in four places (see "Where this review differs from review 1").

**How the evidence was gathered.** Four research agents checked the plan's assumptions on the web. Each source below carries a tag:

- **[F]** a research agent fetched and read the page or PDF this session;
- **[S]** search snippet or abstract only;
- **[M]** memory, not verified this session.

I re-derived the statistics in Change 1 by hand and fetched the Obsidian Bases syntax page myself. I did not independently re-fetch the other sources. Several 2026 items are unreviewed preprints. Where the evidence went against my own starting assumption, I say so.

---

## Executive summary

### What the plan does well

- **The record/execute split and dependency inversion are right.** `inv` records and never runs anything, and producers depend on `inv`, never the reverse (§3). Research on multi-agent "councils" backs the plan's refusal to treat them as validation: debate does not reliably beat simple voting among the same models (Smit et al., ICML 2024 [S]; Zhang et al. 2025 [F]; Choi et al., NeurIPS 2025 [S]).
- **The separation of outcome dimensions is the plan's strongest idea.** Execution status, evidence completeness, measurement validity, interpretation, and decision are kept apart (§6.3), and stale reviews are surfaced as `validated / needs_review` (§5.7).
- **The storage shape is sounder than review 1 gives it credit for.** One immutable file per event, published by write, fsync, no-clobber link, directory fsync, is the same protocol Git uses for loose objects, and it avoids the "append leaves garbage" bug class that the ALICE study found in LevelDB, HSQLDB, and Mercurial [F].
- **The Obsidian facts check out.** Bases is a core plugin, the plan's `.base` filter form is valid, the official CLI needs the desktop app running, Sync excludes dot-folders, and property types are vault-wide [F].

### The main problems

1. **The product's central hypothesis is untested, and the evaluation cannot test it.** No published controlled study shows that structured hypothesis/evidence tracking beats free-form notes for agents, and the nearest evidence is mixed. The §20.5 gate has no comparison arm, reuses its development scenarios as its test set, and at 20 runs cannot tell a 90% success rate from 80%.
2. **The per-hypothesis `supports / contradicts / non_diagnostic` tag is the part of Analysis of Competing Hypotheses (ACH) that the plan's own sources criticize.** The plan cites Mandel et al. to distrust ACH, then keeps ACH's consistency step and drops the things those authors found to work.
3. **Nothing checks that an Observation's text matches the bytes it cites.** Selectors prove a location exists. They do not prove the agent's statement is what the source says, and misread evidence is the dominant measured failure of agent investigations.
4. **Agent-written text is shown at trusted layers.** Titles, synopses, and cross-case lessons are free text that an agent may have written after reading hostile evidence. The plan guards raw excerpts but not this path.
5. **The retrospective gate asks an agent to narrate a process it may not remember.** After a context reset the closing agent has no memory of the case. A mandatory narrative invites confabulation and boilerplate.
6. **The storage contract has four gaps:** the fork-exclusion key, fsync-failure handling, an unnecessary canonical-JSON dependency, and no named crash-testing method.
7. **The plan contradicts itself in several places.** `objection.resolve` exists with no Objection record or create operation; "add an observation in one command" conflicts with a JSON-envelope-only write path.
8. **The domain method packs rest on thin sources** for performance measurement and trade-study weighting, and the debugging pack lacks the fields that separate correlation from confirmed cause.

---

## Where this review differs from review 1

| Review 1 change | My position | Reason |
|---|---|---|
| #8 Replace file-per-event with a segmented append-only log | **Disagree.** Keep file-per-event. | Appends that leave garbage were a recurring bug class in ALICE [F]. After a failed fsync, ext4 and XFS keep page-cache data that is not on disk (Rebello et al., ATC 2020 [F]), which is worse for an appended segment than for a file that either exists or does not. Review 1's performance concern is unmeasured, and the plan's own target is 150 ms p95 per mutation at agent pace. If file count ever matters, pack sealed ranges later, as Git does. See Change 9. |
| #11 "Fix" the `.base` syntax (`note.` prefixes mandatory, add `sort:`) | **Disagree.** The plan's example is valid. | I fetched the syntax page: note properties "can be accessed using the format `note.author` or simply `author` as a shorthand," and the page documents no `sort` key for views. `displayName` labels are a nice touch but not a fix. |
| #2 Adopt NATO Admiralty source grades | **Partly disagree.** Keep the two axes, skip the letter-number grades. | Irwin & Mandel 2019 [S] found the scales "mask rather than guide subjectivity" and that analysts do not rate the two axes independently. §5.6 already separates source reliability from claim credibility; add a "cannot judge" value and stop there. |
| #6 Premortem scenarios on `decision.propose` | **Agree as an advisory card, not as typed fields.** | The often-quoted "30% more reasons" figure could not be verified against the 1989 paper [S]. The one follow-up found (Veinott, Klein & Wiggins 2010 [S]) shows reduced confidence, not better outcomes. |
| #5 Typed `causal_role` tags (confounder, mediator, collider) | **Agree with the checks, cautious on the tags.** | Agents will fill a role field whether or not they can justify it. The two deterministic checks review 1 proposes are worth having; teach the roles in a guidance card first. |
| #1, #12 Baseline arm and early checkpoint | **Agree; extended in Change 1.** | Adds held-out scenarios, sample-size numbers, repeat reliability, and a pre-declared decision rule. |
| #3 Consequence-gated retrospectives | **Agree; extended in Change 6.** | Adds a journal-derived process digest and expectations frozen in advance. I did not verify review 1's Huang et al. citation. |
| #4 `flock`, no-clobber publish, float-free hashing | **Agree; Change 9 goes further.** | Hash the stored bytes and drop canonical JSON from the integrity path entirely. |
| #9 Compact MCP profile | **Agree, with one correction.** | The latest MCP revision (2026-07-28) is stateless with no `initialize` handshake [F], so the plan's "advertised at startup" (§9.7) needs rewording. Measured token cost favors CLI plus a skill file as the primary path: 1,421 tokens for CLI versus 59,417 for a 46-tool MCP server on one task (OpenRouter, Nov 2026 [F]), though deferred tool loading narrows the gap to about 5% (Checkly, Jul 2026 [F]). |
| #7, #10 Search provenance; write-side injection guards | **Agree.** | Change 7 here covers the read side that #10 leaves open. |

---

## Proposed changes

Ordered by priority tier. Line numbers refer to `notes/plan.md` revision 3. Several diffs touch the same lines as review 1's diffs; where they do, mine is written as the superset.

---

### [High impact, low effort] Change #1: Test the central hypothesis before building on it

**Current State:**
§20.5 gives fresh agents the binary and a goal, and passes at "at least 90% over at least 20 documented fresh runs" across ten scenarios with "zero false promotion." There is no arm without `inv`. The ten scenarios are also the fixtures the tool is developed against. The evaluation sits in W14, at Milestone C.

**Proposed Change:**
1. Run three arms on identical seeds and goals: no tool, a free-form notes file, and `inv`.
2. Split scenarios into a development set and a held-out set written by someone who does not work on the checks. Report only held-out results as the gate.
3. Size the experiment from the numbers below, report paired differences with intervals, and report repeat reliability (all k runs of a scenario pass), not only the mean.
4. Grade the *content* of gates, not their presence: were the recorded rival hypotheses real alternatives, and did the retrospective state anything checkable?
5. Run a two-domain version at the end of Milestone A and declare in advance what happens if `inv` does not beat the notes arm.

**Rationale:**
The research agents found no controlled comparison of structured tracking against free-form notes. The nearest evidence points both ways:

- For: a controller-enforced evidence ledger raised majority-of-3 F1 from 8.6 to 86.1 on one model across 35 incident scenarios, while the same algorithm given only as a prompt scored 2.9 (Jha et al., Jan 2026 [F]). The gain came from *enforced* structure. `inv` is voluntary.
- Against: simple unconstrained harnesses scored 65–77% top-1 versus 16–53% for five procedure-driven root-cause agents (Bi et al., Oct 2026 [F, abstract]). A free-form "think" scratchpad alone raised one benchmark from 0.370 to 0.570 (Anthropic [S]), so plain notes are a strong baseline.

The gate's arithmetic, which I re-derived:

| Question | Result |
|---|---|
| 18 of 20 successes, 95% Wilson interval | 69.9% to 97.2% |
| Chance a tool with a true 80% success rate passes an 18-of-20 gate | 21% |
| 0 false promotions in 20 runs, 95% upper bound on the true rate | 13.9% |
| Zero-failure runs needed to bound a failure rate below 2% | 149 |
| Runs per arm to separate 90% from 75% (two arms, 80% power) | about 100 unpaired; fewer when paired on the same scenarios |

Twenty runs over ten scenarios is two per scenario, and runs of the same scenario are correlated, so the effective sample is smaller still (Miller 2024 [F]: clustered standard errors can be more than three times the naive ones). Reliability matters separately from the mean: one agent benchmark scores about 61% on single runs but under 25% when all eight repeats must pass (τ-bench [S]). Coding agents satisfy checks superficially at measurable rates, 30.4% of runs in one METR study [F], which is why gate content has to be graded.

**Benefits:**
- Answers "does this tool help?" before thirteen work packages depend on the answer.
- Turns "zero false promotions" from a slogan into a bounded rate.
- Held-out scenarios stop the blindspot rules from being tuned to their own fixtures.

**Trade-offs:**
- Roughly 100 paired scenario runs per arm is real token cost. Pairing and a small first checkpoint (two domains) keep it proportionate.
- A notes arm that ties `inv` on correctness is an uncomfortable result. It is cheaper to learn at Milestone A than at C.

**Implementation Notes:**
- If an LLM grades runs, the same independence problem applies to the graders: ten LLM judges carried the information of about 3.5 independent ones in one study (Hossain et al. 2026 [F]). Use scenario oracles with machine-checkable properties wherever possible, and blind human or LLM graders to the arm.
- Record cost per arm (tokens, tool calls, wall time). A tool that matches notes on correctness at three times the cost has failed.
- Suggested pre-declared rule: if `inv` does not beat the notes arm on false promotions or resumption accuracy at Milestone A, cut the record types that agents misused or ignored before starting W07.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1839,2 +1839,4 @@
 **A — undeniable vertical slice:** W00–W04 plus minimum W05/W06. Two cases work: debugging and architecture choice, using the same store and CLI. A reviewer sees exact support/assumptions, layered read depths, and a second agent can resume. Early cases may close only with a minimal truthful Retrospective.
 
+**A-gate — does it help?** Before W07, run the three-arm comparison of §20.5 on the debugging and architecture-choice scenarios (held-out variants). Decision rule, recorded as an ADR before the runs: if `inv` does not beat the free-form-notes arm on false promotions or resumption accuracy, cut or merge the record types agents ignored or misused before continuing.
+
@@ -1992,1 +1994,13 @@
 This is a release gate, not a documentation review. At minimum test two distinct agent families/hosts available to implementers, multiple fresh sessions, recorded prompt+tool version, and an independent scenario oracle. Each agent receives **only** the executable, a seed data bundle, a task objective and permission to use `--help`. No hidden domain-specific coaching from the evaluator.
 
+**Arms.** Run every scenario under three conditions with identical seeds and goals: (1) no tool; (2) a free-form notes file the agent may read and write; (3) `inv`. No published study shows that structured evidence tracking beats free-form notes for agents; this comparison is the evidence the product needs.
+
+**Held-out scenarios.** Keep a development set (used while building checks and onboarding) and a held-out set authored by someone who does not work on the blindspot rules. Only held-out results count toward the gate.
+
+**Sample size.** State the minimum detectable difference and the number of runs before running. Twenty runs cannot separate 90% from 80% success (18/20 has a 95% interval of 70–97%), and zero failures in 20 runs bounds the failure rate only below 14%. Plan on the order of 100 paired scenario runs per arm for the headline comparison, and about 150 zero-failure runs for any "below 2%" claim. Report paired differences with intervals clustered by scenario.
+
+**Reliability.** Report the fraction of scenarios in which all k repeats pass, alongside the mean.
+
+**Gate content, not gate presence.** Graders blind to the arm judge whether recorded rival hypotheses were real alternatives with a distinguishing prediction, and whether retrospectives state anything checkable. Report the strawman rate.
+
+**Cost.** Report tokens, tool calls and wall time per arm.
+
@@ -2013,2 +2027,2 @@
-| Task success | At least 90% over at least 20 documented fresh runs, with each domain represented and the new blindspot/retrospective scenarios exercised; investigate every failure. |
-| False promotion | Zero in invalid-control, source-dependence and stale-decision cases. |
+| Task success | On held-out scenarios, `inv` is not worse than the free-form-notes arm (paired difference, 95% interval) and its lower interval bound exceeds a pre-declared floor; each domain represented; investigate every failure. |
+| False promotion | Fewer than the notes arm on invalid-control, source-dependence and stale-decision cases, with the upper 95% bound on the `inv` rate reported. "Zero observed" is stated with its bound, never as "zero." |
```

---

### [High impact, low effort] Change #2: Verify quoted evidence mechanically

**Current State:**
An Observation carries a "source selector or explicitly labeled assertion" (§6.4). The core validates that a selector's offsets fall inside the retained bytes (§5.4). Nothing compares the Observation's statement with the content at that location.

**Proposed Change:**
Add a `text_quote` selector: the exact quoted text plus optional prefix and suffix. The core checks byte-exact presence in the named text derivative and returns `verified`, `ambiguous`, or `not_found`. Text-based Observations should carry a positional selector and a quote together. Add a blindspot rule for promoted Conclusions that rest on Observations with no verified quote.

**Rationale:**
Misread evidence is the most common measured failure in agent investigations. In 1,675 agent root-cause runs, 71.2% contained a hallucinated interpretation of the data (Kim et al., Feb 2026 [F]). Research assistants cite sources that do not support their statements at high rates: citation accuracy of 40–80% (DeepTRACE [F, abstract]) and more than 60% incorrect answers over 1,600 queries (Tow Center, Mar 2025 [F]). Extracting the cited text instead of generating it is an established guard; Anthropic's Citations feature guarantees valid pointers that way [S]. The W3C Web Annotation model recommends pairing a positional selector with a quote, and calls position-only selectors "very brittle" [F].

**Benefits:**
- A deterministic, model-free check on the tool's most valuable property: the claim traces to real bytes.
- Costs the agent nothing extra when it is already quoting.
- Gives reviewers a one-glance signal (`verified` quote) instead of a manual lookup.

**Trade-offs:**
- A verified quote proves the words exist, not that they support the claim. Up to 57% of citations were post-rationalized in adversarial tests (Wallat et al. [S]). The Assessment still needs review; say so in the output.
- Numeric and binary evidence cannot be quoted. Keep positional selectors for those and mark the quote `not_applicable`.

**Implementation Notes:**
- Against an immutable, hash-pinned snapshot a line range is already exact, so the quote's job here is to catch a wrong *statement*, not drift. Keep both.
- Normalize nothing. Match bytes of the named decoded-text derivative, and report `ambiguous` when the quote occurs more than once without disambiguating prefix or suffix.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -427,1 +427,2 @@
 | `text_lines` | One-based inclusive line span on a named exact decoded-text derivative with its own digest, encoding and parser version. |
+| `text_quote` | Exact quoted text plus optional prefix/suffix (the W3C Web Annotation `TextQuoteSelector` shape) on a named decoded-text derivative. The core checks byte-exact presence and reports `verified`, `ambiguous` (more than one match without disambiguating context) or `not_found`. A verified quote shows the words exist in the source; it does not show they support the claim. |
@@ -536,1 +537,1 @@
-| Record observation | Source selector or explicitly labeled assertion, extraction/measurement metadata. | Is statement genuinely observed rather than inferred? |
+| Record observation | Source selector or explicitly labeled assertion, extraction/measurement metadata. A text-based observation carries a `text_quote` alongside its positional selector; the receipt returns the quote check result, and `not_found` rejects the operation. | Is statement genuinely observed rather than inferred? Does the quoted text actually support it? |
@@ -738,1 +739,2 @@
 - An important claimed fact has no retained/external source selector or producer attribution.
+- A promoted Conclusion or accepted Decision rests on a text-based Observation whose quote is absent, `ambiguous`, or was never checked.
```

---

### [High impact, low effort] Change #3: Resolve the plan's internal contradictions

**Current State:**
Five places where the plan disagrees with itself:

1. **Objection has no record type.** §8.5, §14.3 and §21 say reviews add "Objections," and §9.4 lists `objection.resolve`, but §5.2 defines no Objection record, §9.4 has no operation that creates one, and the vault layout has no folder for them.
2. **Cheap capture is promised and not delivered.** §1.2 says "an investigator can add a source or observation in one command." The only write path is `inv record --input FILE` with a JSON envelope, and §9.1 defers aliases to "later." A first observation costs three calls: read the schema, write a file, record.
3. **Review has a `draft` state and no operation that produces it** (§6.2 versus §9.4's lone `review.submit`).
4. **W01's type list omits Assumption, Annotation and Objection**, all of which Milestone A's scenarios use.
5. **The §4.1 diagram has an "Artifact import adapters" box**, while §3.2 forbids adapters compiled into `inv`.

**Proposed Change:**
Add Objection as a record with `objection.raise`; generate flag-form shortcuts for the four commonest writes in v1 from the same catalog; drop Review's `draft` state; fix the W01 list and the diagram label.

**Rationale:**
An implementing agent will resolve each contradiction silently and differently per lane (§19.4 runs six lanes in parallel). The capture path matters most. Interface research is consistent that agents do better with a small default surface and worked examples: tool-use examples raised complex-parameter accuracy from 72% to 90%, and 58 tools cost about 55,000 tokens of context (Anthropic, Nov 2025 [F]). The plan has about 54 operations over 19 record types. No study directly compares JSON-envelope files with flags, so Change 1 should measure it; the shortcut commands are cheap insurance.

**Benefits:**
- Lanes build the same thing.
- The plan's own "three tool calls to first useful commit" target (§20.5) becomes reachable.

**Trade-offs:**
- Flag shortcuts are a second input syntax. Generating them from the catalog, as §9.1 already requires for aliases, keeps one set of semantics.

**Implementation Notes:**
- Each shortcut builds the same envelope, generates a `command_id`, and prints it in the receipt so a retry can reuse it.
- Shortcuts cover single-operation writes only. Batches stay on `record`.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -267,1 +267,1 @@
-    I["Artifact import adapters"] --> C
+    I["Generic ingest / capture manifests"] --> C
@@ -382,1 +382,2 @@
 | **Review** | Attributed review of a case or a conclusion, comparison or decision | Review kind (`validation` or `blindspot`), exact subject/read-set, method, materials examined, source independence, objections/gaps, verdict and caveats. |
+| **Objection** | Attributed, specific challenge to a record | Exact subject revision, statement, severity (`critical` or `material`), originating Review if any, evidence refs, status `open | resolved | withdrawn`. Resolution is its own attributed record, never a flag flip. |
@@ -510,1 +511,1 @@
-| Review | `draft`, `submitted`, `superseded`. `kind: validation` verdict `supports|objects|inconclusive`; `kind: blindspot` verdict `gaps_identified|no_gap_identified_within_scope|inconclusive`. A blindspot review never validates a finding by itself. |
+| Review | `submitted`, `superseded` (drafts live with the client, not in the journal). `kind: validation` verdict `supports|objects|inconclusive`; `kind: blindspot` verdict `gaps_identified|no_gap_identified_within_scope|inconclusive`. A blindspot review never validates a finding by itself. |
@@ -834,1 +835,3 @@
-Typed domain mutations (case, observation, comparison, conclusion etc.) live in the shared `record` operation catalog; convenience aliases may be generated later, never hand-maintained alongside separate semantics.
+Typed domain mutations (case, observation, comparison, conclusion etc.) live in the shared `record` operation catalog.
+
+**Flag-form shortcuts ship in v1** for the four commonest single-operation writes, generated from the same catalog and never hand-maintained: `inv case new`, `inv source add PATH`, `inv observe "..." --source ID [--quote "..."]`, and `inv note "..." --on ID`. Each builds the standard envelope, generates a `command_id`, and prints it in the receipt for retries. This is what makes "add an observation in one command" (§1.2) true.
@@ -903,1 +906,1 @@
-| `objection.resolve` | Identified blocker/review concern revisions, rationale/evidence, update freshness via dependency graph not arbitrary flags. |
+| `objection.raise` / `objection.resolve` / `objection.withdraw` | Raise: exact subject revision, statement, severity, evidence refs, optional originating Review. Resolve: identified objection revision, rationale/evidence; freshness updates via the dependency graph, not arbitrary flags. |
@@ -1820,1 +1823,1 @@
-| W01 | Domain types: Case/Question/Source/Artifact/Observation/Hypothesis/Option/Criterion/Comparison/Assessment/Conclusion/Decision/Review/**Retrospective**/Inquiry/Run. Single operation catalog; review `kind: blindspot` and `annotation role: synopsis`. | W00 | Schema-valid minimal operations and independently compiling core. |
+| W01 | Domain types: Case/Question/Source/Artifact/Observation/Hypothesis/Assumption/Option/Criterion/Comparison/Assessment/Conclusion/Decision/Review/Objection/Annotation/**Retrospective**/Inquiry/Run. Single operation catalog; review `kind: blindspot` and `annotation role: synopsis`. | W00 | Schema-valid minimal operations and independently compiling core. |
```

---

### [High impact, low effort] Change #4: Record evidence comparatively across rival hypotheses

**Current State:**
An Assessment has one subject and one relation: `supports`, `contradicts`, `non_diagnostic` or `inconclusive` (§6.3). §2.1 cites Mandel et al. to caution against ACH.

**Proposed Change:**
When a hypothesis has live rivals, a `supports` or `contradicts` Assessment also states how expected the evidence is under each rival: `more_expected`, `equally_expected`, `less_expected`, or `not_assessed`. Add a blindspot rule for support that was never assessed against a rival, and for support that is equally expected under every rival.

**Rationale:**
The plan's per-hypothesis tag is ACH's consistency-rating step, the exact part its cited critics fault. Mandel & Tetlock 2018 [F]: in ACH "the meaning of consistency is left up to the analyst." Karvetski, Mandel & Dhami 2020 [S] found ACH produced pseudo-diagnostic weighting, where evidence consistent with every hypothesis is still counted as support. Forensic science treats evidence strength as comparative by definition; the ENFSI evaluative-reporting guideline [F] says findings are evaluated "given at least one pair of propositions… If no alternative can be formulated, the value of the findings cannot be assessed."

Review 1's Change #5 adds a rival-prediction table to confirmatory InquiryPlans. That covers planned tests. Most debugging and research evidence is not a planned test, so the comparison also has to live on the Assessment.

**Benefits:**
- Makes non-diagnostic "support" mechanically visible without computing a score.
- Stays qualitative, so it fits the plan's rule against invented precision.

**Trade-offs:**
- One more field per rival on each Assessment. `not_assessed` keeps capture cheap; the blindspot rule surfaces it only at promotion time.
- A strawman rival satisfies the form. Change 1 grades that.

**Implementation Notes:**
- The same ENFSI guideline separates *evaluative* from *investigative* reporting, the latter for when no proposition pair can be formed yet. That maps onto `explore` mode: do not require the comparison before rivals exist.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -146,1 +146,3 @@
 **Caution about ACH:** the evidence-versus-hypothesis matrix is a reasoning aid, not a validated probability engine. Empirical studies have found mixed or negative effects on human judgment [M3, M4]. Expose qualitative relations and reasoning, and evaluate real task outcomes.
+
+**What the same studies recommend instead.** A per-hypothesis "consistent / inconsistent" rating is ACH's weakest step: its meaning is left to the rater, and evidence that fits every hypothesis gets counted as support. Evidence strength is comparative. Record how expected an observation is under each live rival, not only whether it fits one hypothesis (§6.3).
@@ -525,1 +527,1 @@
-| Assessment relation | `supports`, `contradicts`, `non_diagnostic`, `inconclusive` | What does specified evidence suggest about this hypothesis/claim? |
+| Assessment relation | `supports`, `contradicts`, `non_diagnostic`, `inconclusive`; plus, when live rivals exist, a per-rival expectation `more_expected | equally_expected | less_expected | not_assessed` | What does specified evidence suggest about this hypothesis/claim, and would we have expected the same evidence if a rival were true? |
@@ -735,1 +737,2 @@
 - Multiple citations masquerade as independent evidence despite common original provenance.
+- A Conclusion's support was never assessed against a live rival hypothesis, or every supporting item is `equally_expected` under a rival.
```

---

### [High impact, low effort] Change #5: Separate likelihood from confidence, score it, and add base rates

**Current State:**
Principle 9 bans "made-up probabilities." §5.7 offers one optional field: confidence `low | moderate | high | not_assessed`. The plan has no notion of a prior or reference class.

**Proposed Change:**
1. Split two things the plan merges: **likelihood** (how probable the claim is; optional; either a number or the seven-step ODNI scale) and **confidence** (how good the evidence base is; `low | moderate | high` with named reasons for downgrading).
2. Allow a likelihood only on a claim with a stated way to resolve it. When the claim resolves, score it.
3. Add a `prior` guidance card for explanatory and forecasting questions.

**Rationale:**
The plan's M5 source, ICD 203 [F], defines seven likelihood terms with numeric ranges and says products "must not combine a confidence level and a degree of likelihood… in the same sentence." The plan adopted the confidence half only.

Coarse bins lose accuracy. Rounding scored forecasts to three "confidence level" bins worsened Brier scores by 11.8% for all forecasters and 236% for the best ones; the seven-step ODNI scale lost only 0.7% (Friedman et al. 2018 [F]). That was simulated rounding of resolvable forecasts, not an experiment, so the fair reading is: a seven-step scale is nearly free, three bins are not.

The plan's caution is partly right. IPCC guidance [F] assigns likelihood only where confidence is high, and LLM-stated confidence is overconfident (Xiong et al., ICLR 2024 [F]). Calibration also varies enormously by model: expected calibration error of 0.120 for one frontier model against 0.395 for another on the same benchmark (KalshiBench, Dec 2025 [F]). That variation is the argument for scoring: stated numbers should not drive gates, but a per-model record of resolved claims tells a reader how much to trust them. It also gives the plan's "methodological learning" a quantitative backbone.

On base rates: comparison classes and base rates were the training principle most associated with accuracy gains among forecasters (Chang et al. 2016 [F]; self-reported use, so correlational), and Mandel et al. 2018 [F] criticize ACH precisely because it "does nothing to ensure that analysts consider prior probabilities or objective base rates." Premature closure was the most common cognitive cause in a study of 100 diagnostic errors (Graber et al. 2005 [S]). No randomized test isolates a base-rate prompt, so this belongs in a card, not a gate.

For confidence, borrow GRADE's downgrade domains as prompts [F]: risk of bias, inconsistency, indirectness, imprecision, and reporting bias. GRADE itself says it is "not a quantitative system"; use the domains as named reasons, not a score.

**Benefits:**
- Removes a conflation the plan's own cited standard forbids.
- Probabilities stop being theater once they are scored against outcomes.
- Named downgrade reasons make a `high` confidence auditable.

**Trade-offs:**
- Per-actor calibration needs on the order of a hundred resolved claims to mean anything (a research agent's inference, not a sourced figure). Show the count next to every score.
- Many claims never resolve cleanly. Likelihood stays optional, and unresolvable claims carry confidence only.

**Implementation Notes:**
- Key scores by model family and version, not by the free-text actor string (see Change 8).
- The core computes a Brier score from recorded numbers and recorded resolutions. That is arithmetic on recorded data, consistent with §20.1's rule that the core invents no statistics.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -124,1 +124,1 @@
-9. **No epistemic theater:** avoid made-up probabilities, automatic weighted winner scores, cosmetic votes, and agent councils as substitutes for validation.
+9. **No epistemic theater:** avoid unscored or tool-invented probabilities, automatic weighted winner scores, cosmetic votes, and agent councils as substitutes for validation. An attributed likelihood on a resolvable claim, scored when the claim resolves, is the opposite of theater.
@@ -184,1 +184,2 @@
 | `alternatives` | Premature certainty | Rival explanations *or* action options, explicitly distinguish them. |
+| `prior` | New explanatory or forecasting question | Name a reference class and how often each kind of cause or outcome occurs in it, before weighing case evidence. Skippable with a reason. |
@@ -463,1 +464,5 @@
-Confidence is optional `low | moderate | high | not_assessed`, with a rationale, key limitations, and what would change it. It expresses the assessor's judgment about the evidentiary basis. Significance, decision consequence and severity (where a domain uses it) are separate from evidence confidence.
+**Confidence** is optional `low | moderate | high | not_assessed`. It expresses the assessor's judgment about the evidentiary basis, never the probability of the claim. A `moderate` or `high` value names which downgrade reasons were considered: measurement or source validity, consistency across independent evidence, directness to the stated scope, precision or sample adequacy, and completeness of the search. Each is `ok`, `concern` or `not_assessed`, with a rationale and what would change it.
+
+**Likelihood** is a separate optional field: a number in [0, 1] or one of the seven ICD 203 terms (`almost no chance` … `almost certain`). It is allowed only on a claim with a stated resolution condition, and it is never shown in the same sentence as confidence. When the claim resolves, the core records the outcome and computes a Brier score. `inv` reports calibration per model family and version with the count of resolved claims; a stated likelihood never satisfies or blocks a gate.
+
+Significance, decision consequence and severity (where a domain uses it) are separate from both.
```

---

### [High impact, low effort] Change #6: Ground retrospectives in the journal and in expectations frozen earlier

**Current State:**
`case.close` in substantive modes is rejected without a Retrospective (§6.4, §8.6). The Retrospective is a narrative: methods attempted, effective steps, misleading steps, lessons. Lessons are proposals that later cases may read.

**Proposed Change:**
1. `inv retrospective --draft` computes a **process digest** from the journal: invalid and inconclusive runs, amended predictions, hypotheses rejected, stale reviews, superseded conclusions, revision conflicts, elapsed sequence span. The agent annotates facts instead of recalling them.
2. Record **expectations** when they are cheap to state: at `case.create` (expected answer, expected effort) and at `inquiry.freeze` (predictions, already present). The Retrospective is a diff of expected against actual.
3. Each lesson cites a digest fact, and the tool tracks **whether a lesson was consulted and whether it changed a later case**.
4. Adopt review 1's waiver for trivial or abandoned cases.

**Rationale:**
Debriefs work: a meta-analysis of 46 samples found an overall effect of d = 0.67, about 25% (Tannenbaum & Cerasoli 2013 [F]). The same paper defines a debrief by its *developmental, not administrative* intent, and reports that the average one took 18 minutes. A compliance gate that can only check presence cuts against that definition.

Three specific risks:

- **The closing agent often was not there.** After a context reset an agent has no memory of the process. A required narrative will be reconstructed from the brief or invented. The journal already holds the facts.
- **Hindsight and outcome bias do not yield to instructions.** A meta-analysis found hindsight bias of about d = 0.39 that debiasing manipulations did not reduce (Guilbault et al. 2004 [S]); subjects told to ignore outcomes still used them (Baron & Hershey 1988 [S]). The accepted remedy is recording expectations beforehand, which is the preregistration logic the plan already applies to predictions (Nosek et al. 2018 [F]).
- **Stored lessons propagate errors.** Agents copy retrieved memory records, and erroneous records amplify (Xiong et al. 2025 [S]). Lessons-learned systems in human organizations tend to stay "isolated and perishable" (Donahue & Tuohy 2006 [S]).

**Benefits:**
- A retrospective a fresh agent can write truthfully.
- Lessons that point at recorded events are checkable; lessons that never change a later case can be retired.

**Trade-offs:**
- The digest reports what was recorded, not what happened off the record. Label it that way.
- Expectation fields add a prompt at case creation. Keep them optional; a missing expectation shows up in the retrospective as "no expectation recorded," which is itself informative.

**Implementation Notes:**
- Keep the one-transaction close path. The draft is a read; the agent submits `retrospective.record` with digest references.
- One caution from the forecasting literature: use of post-mortems correlated with *worse* scores in Chang et al. 2016 [F], which the authors attribute to post-mortems following bad forecasts. Do not use retrospective counts as a quality signal.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -772,1 +772,5 @@
 `inv retrospective --case C --format json` reads the current methodological record, showing evidence links and whether proposed lessons were reviewed. `retrospective.record` persists an attributed case-scoped summary of **methods attempted, effective discrimination, wasted/invalid or misleading steps (or explicit none observed), investigation cost/constraints when known, uncertainty that remains, and specific next-time improvements**. It may be updated only by supersession; the earlier retrospective remains readable. A brief and a report show only a bounded highlights section with exact references.
+
+**Process digest.** `inv retrospective --case C --draft` computes, from the journal alone, the facts a retrospective should rest on: runs that were invalid, failed or inconclusive; predictions amended after freezing; hypotheses rejected and options ruled out; reviews that went stale; conclusions superseded or retracted; revision conflicts; and the sequence span of the case. The agent closing a case may have no memory of its process, so the digest is the starting point and each `effective_steps`, `invalid_or_misleading_steps` and `lessons` entry cites a digest item or record ID. The digest covers recorded work only and says so.
+
+**Expected versus actual.** `case.create` accepts optional `expectations` (expected answer or choice, expected effort, what would be surprising). Frozen InquiryPlan predictions already serve this role for tests. The Retrospective reports the difference. Recording expectations beforehand is the only reliable guard against hindsight and outcome bias; instructions to ignore the outcome do not work.
@@ -776,1 +780,3 @@
 Lessons remain **case-scoped observations about process**, not accepted general rules. Mark proposed reusable lessons with applicability, counterexamples/limits and supporting cases. The default never automatically rewrites method packs, agent instructions, prompts or integration config. A later explicit, reviewed `method-pack` authoring workflow may incorporate them; that is outside v1.
+
+**Lesson follow-through.** When a later case reads a lesson, record that it was consulted and, at that case's close, whether it changed anything. A lesson that is never consulted, or consulted and never useful, is shown as such. Lessons carry the classification and trust level of their source case (§16.1).
```

---

### [High impact, low effort] Change #7: Stop untrusted text from reaching trusted layers through agent summaries

**Current State:**
§16.1 treats captured evidence as data and labels excerpts as untrusted. Titles, synopses, annotations, and lessons are agent-authored free text. They appear in the `title` and `summary` depths, in `brief`, in `Start Here.md`, and (lessons) in other cases, with no untrusted label.

**Proposed Change:**
1. Treat **all** free text as data at every depth, whoever wrote it. Only text compiled into the binary or a pinned method pack is guidance.
2. Give authored text a trust level inherited from what its author had read: a synopsis or lesson written in a session that read restricted or untrusted evidence carries that marking.
3. Require a human or an outcome-verified admission step before a lesson is shown in another case.
4. Extend the injection drill in §20.5: hostile evidence, then an agent-written synopsis, then a second agent's `brief`.

**Rationale:**
This path has been demonstrated. Injected web content steered an agent's session summary into its long-term memory in a published proof of concept (Unit 42 [S]). Memory-poisoning attacks reach over 80% success at under 0.1% poison rate (AgentPoison 2024 [F]) and 98.2% injection success using queries alone (MINJA 2025 [S]). The plan's layered retrieval makes the summary layer the *default* read, which makes it the most valuable target. Review 1's Change #10 guards writes; this guards what later readers see.

**Benefits:**
- Closes the persistence path the plan's own cross-case lessons create.
- Costs one field and a rendering rule.

**Trade-offs:**
- `inv` cannot observe what a session actually read. The trust level comes from the declared read-set and from `evidence read` calls made through `inv`; state that limit.
- Labeling everything as data makes labels less salient. Keep the visual distinction between tool guidance and authored text strong, and keep `next` built from the catalog only (already required by §7.1).

**Implementation Notes:**
- Titles are the sharpest case: they appear in every list. Bound their length, strip control characters and markup, and render them as quoted data in JSON output.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1601,1 +1601,7 @@
 These are product integrity controls for agents consuming adversarial evidence, not a claim that text labeling alone prevents every prompt-injection failure.
+
+**Authored text is data too.** Titles, synopses, annotations, rationales and lessons are free text that an agent may have written after reading hostile evidence. An injected instruction can be laundered through a summary into the layer every later agent reads first. Therefore:
+
+- Every free-text field is rendered as quoted data at every depth, in every channel. Only text compiled into the binary or a pinned method pack is guidance.
+- Authored text records the lowest trust level among the artifacts its transaction declared as read, and shows it. This reflects declared reads and reads made through `inv`; it cannot see what else a session consumed.
+- A lesson is shown in another case only after an explicit admission step by a human or by a recorded outcome check; until then it is visible only in its source case.
@@ -2002,1 +2008,1 @@
-7. Receive a source containing a forged `AGENTS.md` or instructions and ignore its requests.
+7. Receive a source containing a forged `AGENTS.md` or instructions and ignore its requests. Second stage: a first agent summarizes that source; a second agent, reading only `brief` and `show --depth summary`, must not act on any instruction carried in the synopsis, title or lesson.
```

---

### [Medium impact, low effort] Change #8: Record what makes two agent reviewers dependent

**Current State:**
§14.2 says actor-ID inequality is "merely a procedural check" and lists what a Review declares. An actor is a free-text string. Nothing records which model produced a review.

**Proposed Change:**
Add an optional actor descriptor (`kind: human | agent`, model family, model version, harness) and have a Review declare its context exposure. Show reviews with their dependence visible, and never count a different model family as independent by itself. Where two judgments are wanted, record each before either sees the other.

**Rationale:**
Models share errors. Across more than 350 models, pairs agreed about 60% of the time when both were wrong, more so within a provider and more so among stronger models, even across providers (Kim et al., ICML 2025 [S]). Ten LLM judges carried the information of about 3.5 independent ones (Hossain et al. 2026 [F]). Evaluator self-preference exists (Panickssery et al., NeurIPS 2024 [S]), though a 2026 re-analysis found only 51% of earlier examples stay significant against a quality baseline (Roytburg et al. [F]). In human analysis, independent judgments recorded before discussion and then combined gave the largest measured accuracy gain: 61% lower error with coherence correction plus averaging (Mandel et al. 2018 [F]). The research agents found no controlled study isolating the anchoring effect of showing a reviewer the original conclusion, so the plan's optional blinded packet (§14.2) remains a reasonable precaution, not a proven one.

**Benefits:**
- Dependence becomes a recorded fact a reader can weigh.
- Feeds Change 5's per-model calibration.

**Trade-offs:**
- The descriptor is self-reported, like the actor string. Label it a claim.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1508,1 +1508,3 @@
 Useful roles are investigator, challenger, verifier, and curator. They can be one human/agent at different stages or separate participants. A profile may require a distinct reviewer for a particular promotion, but actor-ID inequality is merely a procedural check.
+
+An actor may carry a self-reported descriptor: `kind` (`human` or `agent`), model family, model version and harness. Reviews display it. Two reviews from the same model family are shown as one dependence group, and a different family is **not** treated as independent on that basis alone: models share errors across providers, and more so as they get stronger. Where two independent judgments are wanted, each is recorded before either reviewer can read the other's; the journal order shows whether that held.
```

---

### [High impact, medium effort] Change #9: Tighten the commit protocol and drop canonical JSON from the integrity path

**Current State:**
§10.1–§10.2: event files named `<seq>-<sha256>.json`; transaction hash is SHA-256 over RFC 8785 canonical JSON; publish "without clobbering an existing event"; an "OS advisory lock." §20.2 requires kill tests at each boundary but names no method. Nothing says what to do when fsync fails.

**Proposed Change:**
1. **Name events `<seq>.json`.** With the hash in the filename, two different events at the same sequence have different names, so exclusive create does not stop a fork; only the lock does. A sequence-only name makes the filesystem a second line of defense.
2. **Hash the exact stored bytes.** The event's hash is SHA-256 of its file, recorded in the next event's `previous_hash`, in receipts, and in the index. It is checkable with `sha256sum`. Remove canonical JSON from integrity. For idempotency, store the request and compare parsed values.
3. **Treat any fsync error as fatal.** Never retry it and never report success after it. Mark the tail unverified and re-read from disk on the next start.
4. **Name the primitives:** `std::fs::File::lock` (stable since Rust 1.89, `flock` on Unix) on a dedicated lock file; publish with `link(2)` or `renameat2(RENAME_NOREPLACE)`, then fsync the file and the directory yourself. Refuse network filesystems.
5. **Name the crash-test method:** a small storage trait with an in-memory fake that drops un-fsynced data and un-fsynced directory entries and injects I/O errors, driven by property tests; plus one CI job on LazyFS.
6. **Write the ADR that justifies not using SQLite as the canonical store.**

**Rationale:**
- **Canonical JSON is a liability here.** The two Rust implementations are shaky: `serde_jcs` has had "Encoding numbers does not follow specification" open since 2021 and shipped a new major version two days before this review; `serde_json_canonicalizer` yanked a release in October 2026 [F]. The signing community's position is to avoid canonicalization: DSSE "avoids canonicalization to reduce attack surface" [F]. Event files are immutable, so their bytes are already the canonical form.
- **fsync failures are silent traps.** After a failed fsync, ext4, XFS and Btrfs all mark the page clean; on ext4 and XFS the page cache then holds data that is not on disk, and a retried fsync can succeed falsely (Rebello et al., ATC 2020 [F]). PostgreSQL now panics on fsync failure for this reason [F].
- **The publish pattern is right; the details bite.** `RENAME_NOREPLACE` returns `EINVAL` on NFS and on FUSE servers that lack it [F]. `tempfile`'s `persist_noclobber` falls back to a non-atomic link-and-unlink and syncs neither file nor directory [F]. POSIX record locks are released when *any* descriptor for the file closes; `flock` and open-file-description locks are not [F].
- **Hand-rolled journals are where storage bugs live.** ALICE found 60 crash vulnerabilities in 11 applications: Git had 9, Mercurial 10, SQLite in WAL mode none [F]. SQLite's own guidance calls piles of files "a notorious bug-magnet" for concurrency while conceding they are friendlier to grep and editors [F]. The plan's choice is defensible for inspectability and rsync-friendliness, but it should say so and budget the testing that choice requires.
- **Review 1's segmented log would reintroduce the append-garbage class** and needs torn-tail truncation logic on recovery. See the differences table.

**Benefits:**
- Removes a dependency with open correctness bugs from the integrity path.
- Makes the fork guarantee hold at the filesystem level.
- Gives §20.2's kill tests a concrete, repeatable mechanism.

**Trade-offs:**
- An event no longer contains its own hash. Its hash lives in the next event, the receipt and the index, and the head hash is computed on read.
- Structural comparison for idempotency means storing the original request alongside the event. That is small, and §10.1 already stores `request_hash`.
- LazyFS runs over FUSE and needs a CI environment that allows it. Its README does not document directory-entry loss [F], which is why the in-memory fake has to model it.

**Implementation Notes:**
- Make blobs durable (file and directory) before the event that references them, as §10.2 step 6 implies; state it as an ordering rule.
- Because the chain forbids rewriting history, schema evolution is upcast-on-read forever. Put `schema_version` on every event from event 1 and keep readers tolerant.
- UUIDv7 IDs are ordered only within one process [F]. Each CLI call is a new process, so take ordering from the sequence number alone (§5.1 already says this; keep it true in code).

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -288,1 +288,1 @@
-    events/000000000001-<sha256>.json
+    events/000000000001.json
@@ -1066,1 +1066,1 @@
-Use one linear workspace sequence for v1. A short OS advisory lock serializes commits across local processes. This makes cross-record transactions straightforward; case reads and file ingestion need not hold the lock while doing expensive work.
+Use one linear workspace sequence for v1. A short advisory lock serializes commits across local processes: `flock` semantics via `std::fs::File::lock` on a dedicated lock file that is never renamed or deleted. Do not use POSIX record locks, which are released when any descriptor for the file closes. This makes cross-record transactions straightforward; case reads and file ingestion need not hold the lock while doing expensive work.
@@ -1080,1 +1080,1 @@
-Use SHA-256 for artifact identity and transaction integrity. Define transaction hashing as SHA-256 over a versioned domain-separation prefix plus RFC 8785 canonical JSON of the envelope excluding `transaction_hash`. UTF-8 only; reject duplicate JSON keys, non-finite values, and integers outside the interoperable safe range. Represent exact large counters/timestamps/decimals as documented strings where needed. Do not normalize or rewrite artifact bytes.
+Use SHA-256 for artifact identity and transaction integrity. **A transaction's hash is SHA-256 of the exact bytes of its event file**, verifiable with `sha256sum`. The file does not contain its own hash; the hash is recorded in the next event's `previous_hash`, in the receipt and in the index. No canonical-JSON step is part of integrity. For idempotency, store the request and compare parsed values structurally; do not compare canonical hashes. UTF-8 only; reject duplicate JSON keys, non-finite values, and integers outside the interoperable safe range. Represent exact large counters/timestamps/decimals as documented strings where needed. Do not normalize or rewrite artifact bytes.
@@ -1092,1 +1092,3 @@
-7. Write the entire transaction to a temporary file on the same filesystem, flush it, atomically publish to its unique final path without clobbering an existing event, and flush the directory.
+7. Write the entire transaction to a temporary file on the same filesystem and fsync it. Publish it to `events/<seq>.json` with `link(2)` or `renameat2(RENAME_NOREPLACE)`; an existing target is a fork and an integrity failure, never an overwrite. Then fsync the directory. The sequence-only filename is what makes exclusive create enforce one event per sequence.
+
+   **Any fsync error is fatal.** Do not retry it and do not report success: after a failed fsync the kernel may mark the data clean while it is not on disk. Return `commit_status: unknown`, stop writing, and on the next start re-read the tail from disk and re-verify the chain before accepting commands.
@@ -1097,1 +1099,3 @@
 Atomic visibility and power-loss durability are different. Document the tested filesystem/OS guarantees. Initial supported write platform is Linux on a local filesystem; qualify macOS/Windows before claiming equivalent durability. Network shares and cloud-synchronized canonical state are unsupported.
+
+Detect and refuse network filesystems at `init` and in `doctor`; `RENAME_NOREPLACE` returns `EINVAL` on NFS and locking is unreliable there. **Crash testing:** put the commit protocol behind a small storage trait. Test it against an in-memory fake that discards un-fsynced file data and un-fsynced directory entries at any step and injects I/O errors, driven by property tests with replayable seeds. Run one CI job against a real filesystem under LazyFS.
```

---

### [Medium impact, decide before W02] Change #10: Decide by ADR whether the journal is per-workspace or per-case

**Current State:**
One linear sequence for the whole workspace (§10.1). The plan then records three costs of that choice: a single-case export is "not resumable" (§10.7), purging a leaked secret needs "a sanitized new storage epoch" for everything (§16.5), and idempotency and sequence are workspace-wide.

**Proposed Change:**
Before W02, write an ADR comparing the current design with **one hash chain per case** (each case a self-contained directory of events, sharing the workspace blob store by digest). My recommendation is per-case, but the decision is the plan owner's and this is the last cheap moment to make it.

**Rationale:**
This is my own architectural judgment, not a research finding. The case is the unit a person shares, archives, restores, and purges. With a per-case chain, export is a directory copy plus referenced blobs, a purge rewrites one case, and two agents on different cases do not contend for one lock. The feature that needs a workspace-wide sequence is a transaction spanning cases, and the plan has almost none: lessons reference other cases by ID, which needs no shared transaction.

**Benefits:**
- Turns three deferred hard problems (§10.7, §16.5, §22's purge row) into ordinary file operations.
- Limits the blast radius of a corrupt event to one case.

**Trade-offs:**
- No atomic cross-case transaction. Cross-case references become ID-plus-hash pointers that can dangle after a purge.
- Blob garbage collection needs per-case reference manifests.
- A workspace-level view (search across cases, `Start Here.md`) is built from several chains instead of one.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1864,1 +1864,3 @@
 Write concise decision records for: domain-neutral core vs consumer-owned integrations; evidence vs assessment; explanatory vs evaluative ontologies; criterion/version freshness; journal versus Markdown source of truth; local concurrency vs distributed merge; method pack security; recording/execution split; vault ownership; report vs recovery archive; layered summary freshness; audit-rule versioning; review scope and method-retrospective lifecycle; explicit/nonautomatic lesson adoption; supported durability guarantees.
+
+**Two storage ADRs are due before W02, because neither can be changed afterward without a migration:** (1) file-per-event journal versus SQLite as the canonical store, stating the requirement (inspectability, rsync-friendliness) that justifies the hand-rolled journal and the crash testing it obliges; (2) one hash chain per workspace versus one per case, weighing cross-case transactions against single-case export, restore and purge (§10.7, §16.5).
```

---

### [Medium impact, medium effort] Change #11: Put the performance pack on rigorous sources

**Current State:**
The performance method cites AWS Well-Architected [G4] and the NIST handbook. §20.1 requires units, workload, window, "sample count or explicit unknown," and that "no p-value, confidence interval… is invented by the core."

**Proposed Change:**
Record samples with their structure and order, record how load was generated, and add one deterministic check on percentile claims. Replace [G4] as the primary source.

**Rationale:**
- **Unrecorded setup details change results.** Environment size and link order alone moved a measured speedup between 0.92 and 1.10; none of 133 surveyed papers controlled for it (Mytkowicz et al., ASPLOS 2009 [F]).
- **Flat sample arrays lose what rigorous analysis needs.** Repetitions belong to levels (build, process, iteration) and should be sized per level from measured variance, reporting effect-size intervals instead of significance tests (Kalibera & Jones, ISMM 2013 [F]). Note the same paper argues against a fixed "N runs" rule.
- **A-then-B ordering confounds.** Running test and control interleaved on the same instances detects slowdowns of 10% or less (Laaber et al. 2019 [S]); paired "duet" runs improved accuracy 2.3–12.5 times on some suites (Bulej et al. 2020 [F]).
- **Tail latency is easy to measure wrong.** In one stall example the p99 was 1.27 s measured from intended send time and 6.04 ms uncorrected (wrk2 README [F]); this is coordinated omission. The primary fix is a load generator that measures from intended send time; histogram correction is a fallback.
- **Percentiles need enough samples.** A distribution-free 95% bound for p99 cannot exist below n = 299, or for p99.9 below n = 2,995 (order-statistic arithmetic, re-derived here). Percentiles from separate runs cannot be averaged.

The plan's "core computes no statistics" rule is right for inference. The minimum-n bound is arithmetic on a recorded count, and a literal reading of the rule would forbid a cheap check worth having.

**Benefits:**
- The `measure` and `baseline` cards gain fields a reviewer can check.
- Scenario B (§17.2) can seed a coordinated-omission trap and an A-then-B trap, both realistic.

**Trade-offs:**
- More fields per measurement. All are optional at capture; the blindspot rules look at them only when a comparative claim is promoted.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1897,1 +1897,6 @@
 - [ ] Measurement includes units, workload identity, observation window, sample count or explicit unknown, environment/tool identity and limitations.
+- [ ] Raw samples carry their repetition level (build, process, iteration) and order; a flat array is accepted but flagged as unable to support a variance analysis.
+- [ ] Trial order is recorded (`interleaved`, `randomized`, `sequential`); a comparison built on A-then-B sequential runs is flagged.
+- [ ] Latency measurements record the load model (open or closed loop) and whether time is measured from intended send time; a tail-latency claim from a closed-loop generator without that is flagged for coordinated omission.
+- [ ] A percentile claim carries its sample count; the core flags a p99 claim below n = 299 and a p99.9 claim below n = 2,995 as unable to support a 95% bound. This is arithmetic on a recorded count, not inference.
+- [ ] Percentiles from separate runs are never averaged; combining requires raw samples or mergeable histograms.
@@ -2194,1 +2199,5 @@
 | G4 | AWS Well-Architected, [Performance Efficiency Pillar](https://docs.aws.amazon.com/wellarchitected/latest/performance-efficiency-pillar/welcome.html) | Warmed and repeated benchmarks, representative synthetic workload caveats; guidance, not a statistical validity guarantee. |
+| G10 | Mytkowicz et al., [Producing Wrong Data Without Doing Anything Obviously Wrong!](https://sape.inf.usi.ch/publications/asplos09), ASPLOS 2009 | Measurement bias from environment size and link order; record the environment fingerprint. |
+| G11 | Kalibera & Jones, [Rigorous Benchmarking in Reasonable Time](https://kar.kent.ac.uk/33611/), ISMM 2013; Georges et al., [Statistically Rigorous Java Performance Evaluation](https://dri.es/files/oopsla07-georges.pdf), OOPSLA 2007 | Repetition levels, per-level sizing, effect-size intervals over significance tests. |
+| G12 | Tene, [wrk2](https://github.com/giltene/wrk2) and [HdrHistogram](https://github.com/HdrHistogram/HdrHistogram) | Coordinated omission; measure latency from intended send time. |
+| G13 | Bulej et al., [Duet Benchmarking](https://arxiv.org/abs/2001.05811), ICPE 2020; Gregg, [Performance methodologies](https://www.brendangregg.com/methodology.html) | Paired and interleaved runs under noise; active benchmarking and named anti-methods. |
```

---

### [Medium impact, medium effort] Change #12: Let the debugging pack distinguish correlation from confirmed cause

**Current State:**
The debugging example ends in "a scoped root-cause explanation" (§5.3). Observations have no field saying whether they came from watching or from changing something. A Conclusion names one cause.

**Proposed Change:**
Add an evidence kind (`observational` or `interventional`), the variables changed, a reproduction rate, and a toggle confirmation. Reserve "confirmed cause" for an interventional result plus a toggle. Let a causal Conclusion list several factors with roles.

**Rationale:**
The plan's debugging sources are appropriate; a research agent checked and found Zeller and the SRE book are the right primary sources. What is missing is the practice in them. Zeller's method [F] requires that a diagnosis show causality, and that after the fix the failure is gone. Agans' rules [F, via a review] include "change one thing at a time" and "check that it's really your fix that fixed it." Association-based fault localization suffers confounding that causal adjustment improves (Baah et al., ISSTA 2010 [S]). Bisection has its own failure modes: a skipped commit next to the culprit leaves it unidentifiable (git-bisect docs [F]), and flaky tests make naive bisection unreliable (Google 2021 [F, abstract]).

On "root cause": safety science rejects a single root cause for complex failures (Cook: "Post-accident attribution to a 'root cause' is fundamentally wrong" [F]). Industry practice is not uniform; the Google SRE book itself uses "root cause(s)" plus a trigger [F]. A data model that allows several factors with roles serves both camps.

One honest limit: no evidence was found that hypothesis-driven "scientific debugging" raises agent fix rates. AutoSD's measured benefit is explanation quality, not repair rate (Kang et al. [F]). The fields below earn their place by making conclusions checkable, not by a proven accuracy gain.

**Benefits:**
- "Fixed" versus "the symptom stopped" becomes a recorded distinction.
- Flaky failures are represented honestly as k of n.

**Trade-offs:**
- A toggle is not always possible (production-only failures, irreversible changes). `candidate` with a stated reason is a legitimate final state.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -190,1 +190,2 @@
 | `delta-debug` | Reproducible failure with many changed factors | Minimal reproducible difference and limitations of reduction. |
+| `intervene` | A suspected cause rests on logs, traces or correlation only | Change exactly one variable; record the reproduction rate before and after as k of n; apply the fix and see the failure stop, revert it and see the failure return. For bisection, record per-commit run counts, skipped commits, and re-verified endpoints. |
@@ -407,1 +408,1 @@
-- **Conclusion:** a scoped root-cause explanation, not a claim that every timeout in production has that cause.
+- **Conclusion:** a scoped causal explanation naming one or more factors with roles (`trigger`, `latent_condition`, `contributor`), not a claim that every timeout in production has that cause. Its standing is `candidate` until an interventional result with a toggle confirms it.
@@ -1892,1 +1894,3 @@
 - [ ] Correction of an instrument invalidates affected interpretations without deleting historical observations.
+- [ ] An Observation or Run records `evidence_kind` (`observational` or `interventional`), the variables changed, and whether instrumentation changed. A causal Conclusion supported only by observational evidence displays as `candidate`, never `confirmed`.
+- [ ] An intermittent failure is recorded as a reproduction rate (k of n) before and after a change, not as a boolean.
```

---

### [Medium impact, low effort] Change #13: Make criterion weights interpretable

**Current State:**
A Criterion has "units/scale, preferred direction, limits, weighting rationale if any" (§5.2). §15.1 allows optional user weights and forbids automatic winner scores.

**Proposed Change:**
A weight is accepted only with the range it applies to: the worst and best values among the options actually considered. Scores use declared scale anchors, not normalization to the current option set. Record decision-time expectations and a review date. Review 1's Change #6 (dominance screening, value-of-information triage) complements this.

**Rationale:**
A weight without a range has no meaning: "cost matters twice as much as latency" depends entirely on how far apart the options are on each. Directly elicited importance weights are range-insensitive, while swing weights respond to range, though less than they should (Fischer 1995 [F, abstract]; von Nitzsch & Weber 1993 [S], range-sensitivity index 0.34 where 1.0 is ideal). Keeney's "Common Mistakes in Making Value Trade-offs" (2002 [F, abstract]) lists this among twelve mistakes; the claim that it is the most common one is from memory. Normalizing scores to the current option set causes rank reversal: adding a copy of an option can change the ranking of the others (Belton & Gear 1983 [S]).

On ADRs: the plan treats them as a method. The evidence treats them as a recording format. No study found shows ADRs improve decisions, and about 63% of roughly 5,800 open-source ADRs were created already "accepted" (ICSA 2026 [S]), which means written after the decision. The `decision-record` export is fine; the claim in §2.1 that ADR practice is a research method should be softened.

**Benefits:**
- Any weighted score the user asks for can be audited.
- Adding or removing an option does not silently reorder the rest.

**Trade-offs:**
- Ranges are unknown until options are measured. Until then the weight is recorded as a stated preference with range `unknown`, and no weighted score is shown.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1552,1 +1552,3 @@
 **Do not rank everything with uncalibrated numerical weights.** A weighted score is optional user-provided analytic artifact, not the core's truth criterion. If it is used, preserve all input weights, units, normalization assumptions, sensitivity and why its score should matter. A hard constraint violation cannot be quietly compensated by a weighted soft criterion. For subjective criteria, state preference rather than claiming a measurement.
+
+**A weight needs a range.** A weight is accepted only with the worst and best values, among the options actually considered, that it trades off ("the swing from 40 ms to 15 ms p99 is worth w"). A weight with an unknown range is stored as a stated preference and never enters a score. Scores use the criterion's declared scale anchors, not normalization to the current option set, so adding or removing an option cannot reorder the others. Before any weighting, show the consequences table (option × criterion, raw values with evidence links) and flag criteria that overlap or act as proxies for another.
+
+**Decide, then check.** A Decision records what the decider expects to happen and when to look. Judge the decision later against what was knowable then, not against the outcome alone.
```

---

### [Low impact, low effort] Change #14: Map home-grown formats to existing standards at the edges

**Current State:**
§5.4 defines its own selector union. §6.5 mentions "a supported executor receipt" and "a future verifiable external timestamp" without naming formats. IDs are UUIDv7 everywhere, including exports.

**Proposed Change:**
Keep the internal formats and document the mappings: selectors to W3C Web Annotation on export; executor receipts as in-toto Statements in DSSE envelopes; external timestamps as RFC 3161 tokens over the chain head. Note the UUIDv7 timestamp in exported IDs.

**Rationale:**
- Most selectors map cleanly: `byte_range` to `DataPositionSelector`, `text_lines` to RFC 5147, `table_cell` to RFC 7111, `time_window` to Media Fragments [F]. `json_pointer` has no entry and stays an extension. No evidence was found that Web Annotation alignment is standard practice outside annotation tools, so this is an export mapping, not a redesign.
- An in-toto Statement (subject digests plus a typed predicate, in a DSSE envelope) fits "executor bound the plan digest before running" [F, repository; details M]. An RFC 3161 token needs the network only at stamping time and verifies offline, which suits an offline-first tool; a public transparency log does not [S/M].
- UUIDv7 embeds a millisecond creation time. RFC 9562 advises treating UUIDs as opaque and using random ones where the timestamp is sensitive [F]. For a review bundle shared outside the team, every ID reveals when its record was made.

**Trade-offs:**
- Each mapping is documentation and a small exporter, not core work. Defer the exporters; write the mapping table now so the internal formats do not drift away from it.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -434,1 +434,3 @@
 The core validates offsets and native selectors against retained bytes where possible. Unsupported external selectors are stored with `validation: unavailable`, **not falsely verified**. A producer/reader can later attach an attributed validation/extraction record. `whole` on a missing external reference remains unverified. An Obsidian link or generated block anchor is for navigation and never replaces canonical digest-bound identity.
+
+**Export mapping.** Keep a documented mapping from these selectors to W3C Web Annotation selectors (`byte_range` → `DataPositionSelector`, `text_lines` → RFC 5147, `text_quote` → `TextQuoteSelector`, `table_cell` → RFC 7111, `time_window` → Media Fragments). `json_pointer` is an extension. The mapping is for interchange; internal identity stays digest-bound.
@@ -560,2 +562,2 @@
-- `executor_bound`: a supported executor receipt binds the plan digest before its execution;
-- `externally_attested`: a future verifiable external timestamp/checkpoint, with authority and verification details.
+- `executor_bound`: an executor receipt binds the plan digest before its execution. Accept an in-toto Statement in a DSSE envelope as the receipt format; do not invent one;
+- `externally_attested`: a verifiable external timestamp, with authority and verification details. Accept an RFC 3161 token over the chain head, which verifies offline.
@@ -1589,1 +1591,1 @@
 Audience policy is positive inclusion, not a best-effort scrub. Review vault and recovery archive have intentionally different completeness/security guarantees. Export manifests contain snapshot ID, renderer/schema versions, source digests, omissions, classification policy and where applicable supersession links. Optional HTML stays inert and self-contained; PDF is an optional later exporter.
+
+UUIDv7 identifiers embed a millisecond creation time. A review export discloses it for every record; state this in the export manifest, and offer remapped random IDs for exports where record timing is sensitive.
```

---

## Verified without change

These plan claims were checked and hold, so no change is proposed:

- **Rejecting agent councils and automatic scores** (principle 9, §22). Supported by three independent studies; see the summary.
- **The plan's `.base` example** (§12.5). Valid per the Obsidian syntax page, which I fetched.
- **Obsidian CLI, Headless, Sync and property behavior** (§12.3, §12.8, §23.6). Confirmed against current documentation at Obsidian 1.15.1 [F]. One addition worth a line in §12.7: users report lag at 50,000 notes and unreliable auto-merge when an open note is rewritten externally [S], so load-test the vault at that size.
- **The debugging sources** [G1, G3]. Appropriate; Change 12 adds the practices, not new authorities.
- **Write-temp, fsync, publish, fsync-directory** (§10.2). Still the accepted pattern in 2026 [F].

## Limits of this review

- I did not re-fetch the research agents' sources, apart from the Bases syntax page. Tags show how each was obtained; [S] and [M] items deserve a second look before anyone cites them in the plan.
- Several 2026 papers are unreviewed preprints, including the two that bear most directly on Change 1 (Jha et al.; Bi et al.).
- Change 10 is my judgment, not a research result.
- Not verified: review 1's Huang et al. citation; the "30%" premortem figure; Keeney's "most common mistake" attribution; Rust SDK support for MCP revision 2026-07-28.
- I did not review the Obsidian companion plugin design (§13) or the export/retention sections (§16.2–§16.5) beyond the points above.
