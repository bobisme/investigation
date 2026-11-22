# Critical Review: Investigation Implementation Plan (`notes/plan.md`, Rev. 3)

**Reviewed file:** `notes/plan.md` (Revision 3, 2026-11-21)  
**Review focus:** Epistemic & decision-science methodology, empirical challenges to core assumptions, storage/concurrency/canonicalization soundness, LLM-agent UX & security failure modes, evaluation rigor, and Obsidian integration accuracy.

---

## Executive Summary

### Strengths

`notes/plan.md` is unusually disciplined, coherent, and self-aware compared to typical agent-tooling specs:

1. **Clean architectural boundaries:** Strict dependency inversion (§3.1), execution/recording separation (§3.3), and refusing to parse assistant prose for mutations (§2.2) eliminate whole classes of agent-harness coupling and silent state corruption.
2. **Genuine epistemic honesty:** Separating execution status, evidence completeness, measurement validity, interpretive assessment, and decision acceptance (§6.3); preserving negative/inconclusive results; maintaining `basis_digest` vs. CAS revision (§5.7); and computing effective disposition overlays (`validated / needs_review`) are strong foundations.
3. **Pragmatic local-first design:** Offline Rust core, content-addressed blobs, deterministic byte-budgeted progressive reads (§8.3), and refusal to treat Obsidian Sync or DataviewJS as canonical infrastructure (§12) avoid brittle dependencies.
4. **Honest treatment of ACH limits:** Explicitly warning against treating ACH as a probability engine (§2.1) and avoiding arbitrary weighted-sum decision winners (§15.1) shows strong awareness of epistemic theater.

### Key Areas for Improvement

Challenging the plan's assumptions against empirical literature in judgment/decision science, philosophy of science, LLM reasoning/security, systems engineering, and evaluation statistics exposes **12 high-leverage weaknesses** across five themes:

1. **Methodology gaps (preventing it from being truly top-tier):**
   - **No Severe Testing / Causal Structure (Mayo & Pearl):** Relying on qualitative `supports`/`contradicts` and prose "confounds" omits **counterfactual test severity** (what would have happened if $H$ were false?) and fails to distinguish **confounders** (must control) from **colliders** (filtering on failures *creates* spurious correlation) and **mediators**.
   - **Conflated uncertainty scales:** Collapsing uncertainty into one `low|moderate|high` confidence field violates **ICD 203** (which strictly separates *likelihood* from *analytic confidence*), lacks **GRADE**-style explicit certainty downgrade criteria, and omits **NATO STANAG 2511**'s independent *source reliability* vs. *information credibility* split.
   - **Missing modern decision-science primitives:** Rejecting weighted sums without adding **weight-free Pareto dominance**, **Value-of-Information (VoI) triage**, **Outside-View Reference Class Forecasting (Flyvbjerg)**, and **Premortems (Klein)** leaves evaluative cases under-tooled.
   - **No search provenance / negative-search logging (PRISMA):** Documentary and code investigations cannot distinguish "searched corpus and found no counter-evidence" from "never searched," inviting severe reporting/selection bias.
2. **Unexamined LLM cognitive & security failure modes:**
   - **Intrinsic self-correction & Goodhart's Law:** Making `retrospective.record` a hard gate on `case.close` and relying on same-session `inv guide blindspot` ignores empirical findings (*Huang et al., ICLR 2024*) that LLMs cannot reliably self-correct without external feedback and will emit sycophantic/boilerplate compliance to clear mandatory gates.
   - **Prompt injection targeting valid `inv record` writes:** Text delimiters alone do not stop an agent tainted by hostile raw evidence from executing *schema-valid* `inv record` mutations that corrupt conclusions or retract evidence (*Debenedetti et al., CaMeL, 2025*).
3. **Underpowered & uncontrolled evaluation (§20.5):**
   - Testing 20 runs across 10 scenarios with **zero no-tool baseline control** violates the plan's own scientific standards: 18/20 (90%) has a 95% Wilson CI of `[69.9%, 97.2%]` and cannot even demonstrate that `inv` outperforms plain Markdown notes!
4. **Systems, storage, and protocol hazards:**
   - **One-file-per-event + per-event directory `fsync`** (§10.1–§10.2) causes severe ext4 journal (`jbd2`) thrashing and inode bloat; POSIX `rename()` clobbers targets and `fcntl` locks break on `close()`; and **RFC 8785 (JCS)** has notorious Rust-vs-Python floating-point canonicalization pitfalls.
   - **MCP schema bloat (§9.7):** Exposing ~21 tools with nested operation-union JSON Schemas injects 15k–55k tokens eagerly and degrades LLM tool selection.
5. **Obsidian Bases syntax & v1 scope risk:**
   - The `.base` example (§12.5) omits official `note.` property prefixes and `sort:` vs. `order:` separation; freezing all 18+ entity types in W01 before a cold-agent trial risks locking in schema ceremony.

---

## Proposed Changes

Changes are ordered by priority tier: **High Impact, Low Effort** → **High Impact, High Effort** → **Medium Impact, Low Effort**.

---

### [High Impact, Low Effort] Change #1: Add Paired No-Tool Baseline Controls, Confidence Intervals, and Statistical Power to Cold-Agent Evaluation (§20.5)

**Current State:**  
§20.5 specifies a release gate of *"At least 90% over at least 20 documented fresh runs, with each domain represented"* across 10 base/adversarial scenarios, with **no control group** (no comparison against the same agent solving the same investigation without `inv`) and no uncertainty quantification or power calculation.

**Proposed Change:**  
Restructure §20.5 into a **paired controlled experiment**:
1. Add a **baseline control arm** (same fresh agent, same seed bundle, using plain local files/shell without `inv`) evaluated by the exact same independent scenario oracle on the same tasks.
2. Require **paired difference reporting with 95% confidence intervals** (and cluster-adjusted standard errors where scenarios share seed fixtures), following Evan Miller's *"Adding Error Bars to Evals"* (2024).
3. Increase minimum sample size so domain-level and overall comparisons are statistically powered (e.g., ≥5 independent fresh runs per scenario × 2 arms × ≥2 model families, or an explicit power/minimum-detectable-effect justification) rather than 2 runs per scenario.
4. Add an explicit **net epistemic value gate**: `inv` must not perform worse than the plain-notes baseline on answer accuracy, must achieve a measurable reduction in false promotions/caveat omissions, and must transparently report its token/tool-call overhead relative to baseline.

**Rationale:**  
This is the single starkest internal contradiction in `plan.md`: §2.1 and §17.2 insist on valid baselines, controls, and statistical honesty, yet the plan's own release gate (§20.5) has **no control group** and is severely underpowered. With $N = 20$ runs spread across 10 scenarios (only 2 runs per scenario), an observed 18/20 (90%) pass rate has a **95% Wilson score confidence interval of `[69.9%, 97.2%]`**. Worse, empirical research on structured analytic techniques (*Dhami et al., 2019; Mandel et al., 2018*) repeatedly found that rigid analytical scaffolding can perform *no better than—or worse than—unstructured baselines*. Without a paired plain-notes control, passing 90% could simply reflect an easy fixture where the agent would have scored 95% faster and cheaper without `inv`.

**Benefits:**
- Directly proves whether `inv` actually improves investigative accuracy and handoff fidelity over plain Markdown/shell, rather than measuring fixture easiness.
- Prevents shipping schema friction that hurts agent reasoning while appearing to pass raw completion checks.
- Makes the evaluation gate statistically defensible with tight paired standard errors (Miller, 2024).

**Trade-offs:**
- Roughly doubles evaluation compute/API cost for Milestone C (running both control and treatment arms across ≥5 trials/scenario).
- Requires writing an oracle that grades epistemic outputs identically regardless of whether the agent used `inv` JSON or plain Markdown files.

**Implementation Notes:**
- Design the independent scenario oracle around **observable epistemic assertions** (e.g., did the agent identify the confounded trace, refuse false corroboration, state the hard-criterion reversal trigger, and enable a second fresh agent to resume accurately?) so both arms are graded identically without favoring `inv` formatting.
- Record raw per-trial pass/fail, token counts, wall-clock steps, and false-promotion events as versioned evaluation artifacts; compute paired McNemar / paired bootstrap 95% CIs.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1990,7 +1990,7 @@
 ### 20.5 Cold-agent evaluation
 
-This is a release gate, not a documentation review. At minimum test two distinct agent families/hosts available to implementers, multiple fresh sessions, recorded prompt+tool version, and an independent scenario oracle. Each agent receives **only** the executable, a seed data bundle, a task objective and permission to use `--help`. No hidden domain-specific coaching from the evaluator.
+This is a release gate structured as a **paired controlled experiment**, not a documentation review or an uncontrolled pass-rate scoreboard. At minimum test two distinct agent families/hosts available to implementers, recorded prompt+tool versions, and an independent scenario oracle that evaluates epistemic accuracy identically across arms. Run two paired arms on the exact same scenarios and seed bundles: (A) **control baseline** (fresh agent with standard shell/plain Markdown notes, no `inv`) and (B) **treatment** (fresh agent with **only** the `inv` executable, seed bundle, task objective, and `--help`; no hidden domain-specific coaching).
 
 Four base scenarios plus adversarial variants:
 
@@ -2006,18 +2006,20 @@
 
 Measure at least:
 
 | Measure | Initial acceptance target |
 |---|---|
 | Correct case/object targeting | 100%; no cross-case writes. |
 | Simple first useful commit | Within three tool calls after onboarding/schema access. |
-| Task success | At least 90% over at least 20 documented fresh runs, with each domain represented and the new blindspot/retrospective scenarios exercised; investigate every failure. |
-| False promotion | Zero in invalid-control, source-dependence and stale-decision cases. |
+| Task success (statistical power) | At least 5 independent fresh trials per scenario per arm per model family (e.g., ≥100 treatment + ≥100 paired control runs across 10 scenarios, or documented power calculation for ≥80% power at α=0.05). Report point estimates with **95% confidence intervals** (cluster-adjusted where scenarios share fixtures) and **paired treatment-minus-control deltas**. Treatment must meet ≥90% overall success (lower 95% CI ≥80%) and show non-inferiority or superiority vs. plain-notes control. Investigate every failure. |
+| False promotion (vs. baseline) | Zero in treatment for invalid-control, source-dependence and stale-decision cases, and a statistically documented reduction vs. control arm where baseline false promotions occur. |
 | Lost/duplicate writes | Zero with injected response loss, retries and process termination. |
 | Repairability | Ordinary schema/CAS error resolved without bespoke intervention. |
 | Continuation | Fresh agent accurately states the current answer/choice, its strongest caveat, and next useful observation. |
-| Context efficiency | Measure bytes/tokens and unnecessary calls; title/summary reads reduce output versus full evidence while preserving necessary caveats. A compact 40-record fixture should emit at least 50% fewer bytes than a full-record traversal without worse answer correctness. This is a release *target*, not a claimed result. |
+| Context efficiency & overhead | Measure total tokens, emitted bytes, and tool calls for both arms. Title/summary reads must emit at least 50% fewer bytes than full-record traversal on the 40-record fixture without worse answer correctness; explicitly report `inv` token/call overhead relative to the plain-notes baseline. |
 | Blindspot calibration | All seeded structural gaps detected in matched fixtures; no claim of completeness for unknown semantic gaps. Reviewer can record additional gaps safely. |
 | Learning discipline | Every completed substantive scenario has a linked, useful Retrospective; zero invented failures or auto-adopted method changes. |
 | Domain neutrality | Agents in architecture/research tasks do not need security abstractions or unrelated test-run ceremony. |
 
-These are release targets, not measured results. Don't cheat by embedding fixture-specific solutions in onboarding or source labels. Compare against independent expected evidence/decision properties, not just tool-call completion.
+These are release targets, not measured results. Don't cheat by embedding fixture-specific solutions in onboarding or source labels. Compare against independent expected evidence/decision properties, not just tool-call completion, and publish the paired control-vs-treatment results and confidence intervals alongside any release claim.
```

---

### [High Impact, Low Effort] Change #2: Separate Claim Plausibility/Likelihood from GRADE-Style Evidence Certainty (ICD 203) and Source Reliability from Information Credibility (STANAG 2511) (§2.1, §5.6–§5.7)

**Current State:**  
§5.7 defines a single optional field `confidence: low | moderate | high | not_assessed`, describing it as *"the assessor's judgment about the evidentiary basis,"* while §2.1 cites **ICD 203** [M5]. Meanwhile, §5.6 lists six conceptual separation dimensions (byte integrity, source authenticity, measurement validity, interpretive confidence, reproduction status, independence) in prose, without structuring source reliability separately from datum credibility or providing auditable downgrade criteria for confidence ratings.

**Proposed Change:**  
1. **Enforce the ICD 203 two-axis split in §5.7:** Explicitly distinguish **evidence certainty** (how strong, valid, consistent, direct, and complete the evidence base is — `very_low | low | moderate | high | not_assessed`) from **claim plausibility / calibrated likelihood** (optional bounded probability interval such as `[0.60, 0.85]` or calibrated scale, never conflated with evidence certainty and never fabricated when unassessed).
2. **Anchor evidence certainty in explicit GRADE-style dimensions:** Require `moderate` or `high` evidence certainty assessments to record explicit structured status across five auditable axes: **methodological validity / risk of bias**, **consistency across independent sources**, **directness / applicability to scope**, **precision / sample adequacy**, and **search / reporting completeness** (so an agent cannot rubber-stamp `high` while hiding indirectness or unsearched sources).
3. **Adopt the NATO STANAG 2511 (Admiralty Code) split in §5.6:** Record **source reliability** (origin track record/competence/access) independently from **information credibility** (internal consistency, physical plausibility, and independent corroboration of the specific observation), explicitly warning against diagonal coupling bias (*Irwin & Mandel, 2019*).

**Rationale:**  
- **ICD 203 Tradecraft Standard 2** explicitly prohibits merging likelihood and analytic confidence: you can have **high evidence certainty** that a hypothesis is **very unlikely** (e.g., rock-solid telemetry refuting a suspected memory leak), or **low evidence certainty** in a **plausible** claim. With only a single `confidence` field, agents and humans routinely overload it to mean both "how likely is this true?" and "how good is my data?", producing ambiguous records.
- Empirical studies (*Dhami & Mandel, 2022; Friedman et al., 2018*) show unanchored verbal labels (`low`/`moderate`/`high`) have massive interpersonal interpretation variance unless anchored to explicit rubric criteria (like **GRADE**'s 5 downgrade domains) or optional numerical probability intervals.
- In intelligence evaluation (*Baker et al., 1968; Irwin & Mandel, 2019*), conflating *source reliability* with *information credibility* causes severe "halo" errors (trusting a flawed statement from a prestigious source or dismissing a verifiable datum from an unverified origin).

**Benefits:**
- Eliminates the #1 ambiguity in analytic communication (strength of evidence vs. likelihood of truth).
- Turns `confidence: high` from an uncheckable opinion into an auditable five-factor checklist that `inv blindspots` can cross-check against recorded observations and assumptions.
- Prevents prestigious-source halo bias and supports rigorous documentary/OSINT/engineering source appraisal.

**Trade-offs:**
- Adds small structured subfields to `source.assess`, `observation.add`, and `assessment.add` (all optional during raw capture; structured certainty factors required only when asserting `moderate`/`high` certainty on a promoted claim).

**Implementation Notes:**
- Keep raw capture frictionless: `certainty_factors` and `likelihood_interval` default to `not_assessed` / `null`.
- Add a deterministic `inv blindspots` rule flagging `certainty: high` when any of the 5 GRADE-style factors is `unassessed` or `serious_concern`, or when a single source-reliability rating is mechanically copied onto information credibility without independent corroboration.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -135,7 +135,8 @@
 | Scientific/hypothetico-deductive troubleshooting | Iterate between measured behavior, explanations, tests and causal intervention. | Capture baseline, competing root causes, test setup, valid/invalid outcomes and reproduction limits. | [G1], [G2] |
 | Delta debugging / differential testing | Isolate minimal difference between passing and failing conditions. | Support paired runs, fixed environment descriptions, reduction trials and source-linked claims. | [G3] |
 | Experimental controls and replication | Distinguish noise, confounds and invalid instruments from meaningful effects. | Immutable measurement conditions, controls, run groups, source independence and honest negative results. | [M10], [M11], [G4] |
-| Analysis of competing hypotheses / intelligence methods | Explicit alternatives, linchpin assumptions and diagnostic evidence. | Qualitative relationship matrix, contradictions, scoped standing, no evidence-vote score. | [M1]–[M5] |
+| Analytic standards & evidence grading (ICD 203, GRADE, STANAG 2511) | Separate claim likelihood from evidence certainty; grade evidence on explicit bias/consistency/directness/precision/reporting axes; separate source reliability from datum credibility. | Independent certainty vs. optional likelihood interval; structured 5-factor certainty rubric; decoupled source-reliability and datum-credibility fields; qualitative rival matrix without evidence-vote scores. | [M1]–[M5] |
 | Decision analysis / trade studies | Specify must-haves, criteria, option performance, sensitivity and uncertainty before choosing. | First-class options, criteria, versioned measurements and criterion assessments; conditional decisions. | [G5], [G6] |
 | Architecture decision records | Persist context, decision and consequences; supersede rather than rewrite accepted history. | Immutable accepted decisions and exportable ADR-style reports. | [G7], [G8] |
 | Provenance / reproducible research | Separate provenance of entities, activities, agents, derivations and reports. | Evidence lineage, exact selectors, acquisition context, source quality, transformations and responsible actors. | [M6], [M8], [M9], [G9] |
@@ -449,19 +450,28 @@
 Separate:
 
 - byte integrity;
-- source authenticity/attribution;
+- **source reliability** (origin track record, competence, access — analogous to NATO STANAG 2511 A–F);
+- **information credibility** (internal consistency, plausibility, and independent corroboration of the specific datum — analogous to STANAG 2511 1–6; must be assessed independently of source reliability to avoid diagonal halo bias);
 - measurement validity;
-- interpretive confidence;
+- evidence certainty vs. claim likelihood;
 - reproduction status;
 - independence of sources or methods.
 
 Do not collapse them into one score. `unknown` is a normal state. A new actor ID is attribution, not authentication and not proof of independence.
 
 ### 5.7 Confidence and freshness
 
-Confidence is optional `low | moderate | high | not_assessed`, with a rationale, key limitations, and what would change it. It expresses the assessor's judgment about the evidentiary basis. Significance, decision consequence and severity (where a domain uses it) are separate from evidence confidence.
+Following ICD 203 and GRADE, strictly separate **evidence certainty** from **claim likelihood/plausibility**; never conflate them in one field:
+
+- **Evidence certainty** (`very_low | low | moderate | high | not_assessed`): expresses how solid the supporting evidence and methodology are, backed by rationale, key limitations, what would change it, and—whenever `moderate` or `high` is claimed on a promoted Conclusion—explicit structured ratings across five GRADE-inspired axes: `risk_of_bias_or_validity`, `consistency`, `directness_to_scope`, `precision_or_sample_adequacy`, and `search_and_reporting_completeness`.
+- **Claim likelihood / plausibility** (optional, default `not_assessed`): if the investigator chooses to express probabilistic belief (e.g., for forecasting or decision risk), record an explicit bounded interval `[p_low, p_high]` (e.g., `[0.65, 0.85]`) or calibrated qualitative band with reference basis—never an auto-generated score and never merged with evidence certainty. High evidence certainty can support a *very unlikely* claim (strong refutation).
+
+Significance, decision consequence and severity (where a domain uses it) remain separate from both evidence certainty and likelihood.
 
 Each assessment and review cites exact subject/evidence revisions. Maintain a dependency index. Retraction, material correction, invalidated measurement, changed applicability or unresolved contradiction marks dependent Assessments, Comparisons, Conclusions and Decisions `needs_review` according to declared materiality and relevance.
```

---

### [High Impact, Low Effort] Change #3: Counter LLM Intrinsic Self-Correction Failure & Performative Gate-Gaming in Retrospectives and Blindspot Reviews (§6.4, §8.5, §8.6, §14.2)

**Current State:**  
1. §6.4, §8.6, and §9.4 make `retrospective.record` a **mandatory hard gate** on every substantive `case.close`.
2. §8.5 has the *same* agent invoke `inv guide blindspot` and submit `review.submit(kind=blindspot)` as a "semantic adversarial review," without structurally distinguishing same-context self-critique from context-isolated or externally grounded critique.

**Proposed Change:**  
1. **Replace unconditional mandatory retrospectives with consequence-gated + evidence-grounded retrospectives:** Make `retrospective.record` mandatory only when a case crosses a declared materiality threshold (e.g., promoted Conclusion/Decision, failed/invalidated runs, >N transactions, or opt-in pack policy), and allow an explicit low-friction `retrospective_waived` reason for small/trivial or abandoned cases. Require every recorded lesson in `lessons[]` to link to a concrete **external/recorded signal** (an invalidated run, falsified prediction, retracted source, revision conflict, or recorded blindspot gap), preventing ungrounded prose boilerplate.
2. **Track `context_isolation` explicitly on `review.submit`:** Require every validation or blindspot review to declare `context_isolation: same_session_self_review | fresh_context_unblinded | fresh_context_blinded | external_evidence_backed`. Warn explicitly in `brief`, `blindspots`, and reports that `same_session_self_review` is empirically prone to anchoring and intrinsic self-correction degradation.

**Rationale:**  
- **Huang et al. (ICLR 2024), *"Large Language Models Cannot Self-Correct Reasoning Yet"*** proved empirically that intrinsic self-correction (prompting an LLM to critique its own prior reasoning without *new external feedback* such as execution results, fresh evidence, or independent oracle checks) **fails to improve accuracy and frequently degrades it** by flipping correct judgments to incorrect ones. Furthermore, multi-agent debate without external grounding performed no better than independent sampling.
- **2025 LLM sycophancy & anchoring research** demonstrates that once an agent has committed to a hypothesis in its context window, prompting it to "challenge itself" triggers post-hoc rationalization or sycophantic vacillation rather than genuine falsification.
- **Goodhart's Law on hard documentation gates:** Requiring `retrospective.record` to close *every* non-record case guarantees that agents on routine or short cases will fabricate bland, generic lessons (e.g., *"Always check logs carefully"*) just to satisfy exit code 0, degrading the signal-to-noise ratio of future transferable learning.

**Benefits:**
- Aligns blindspot/review design with actual empirical LLM behavior instead of assuming self-prompting fixes reasoning flaws.
- Prevents "retrospective slop" from polluting reusable lesson corpora while keeping high-signal retrospectives mandatory on consequential or failure-rich cases.
- Makes the difference between cheap same-session self-check and genuine fresh-context/blinded or externally verified challenge unmistakably visible to reviewers.

**Trade-offs:**
- Adds one required enum field (`context_isolation`) to `review.submit`.
- Requires defining clear deterministic criteria for when a case is "material enough" to trigger the default retrospective obligation (configurable in method packs).

**Implementation Notes:**
- Default materiality rule for mandatory retrospective: case has ≥1 promoted Conclusion (`review_ready`/`validated`), ≥1 accepted Decision, ≥1 invalidated/failed Run, or ≥1 retracted/corrected source/observation; otherwise `case.close` permits `retrospective_disposition: waived` with a short reason.
- When `inv blindspots` emits its response, include a structural reminder if all existing blindspot reviews for the subject have `context_isolation: same_session_self_review`.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -542,8 +542,8 @@
 | Recommend/accept a decision | Candidate options and applicable active criteria, hard constraint disposition, material unknowns, rationale, consequences, reversibility/reopen trigger. | Does the trade justify the choice? |
 | Promote conclusion to review-ready | Bounded claim, direct support, applicability, alternative explanation treatment, limitations and inspectable evidence. | Is it convincing? |
 | Validate conclusion | Valid review/read-set, no unresolved critical blockers; method-specific policy obligations met. | Is residual uncertainty acceptable? |
-| Blindspot review (optional by default) | `inv blindspots` checks recorded structure; `review.submit` with `kind: blindspot` captures reviewed scope, gaps and exact read-set. | Did the investigator actively challenge omissions and rival accounts? A clean checklist is not proof of completeness. |
-| Close substantive case | Answer/choice/outcome or explicit inconclusive reason; outstanding questions, limitations, reopen triggers; attributed method Retrospective in same or prior transaction. `record`-only sessions are exempt. | Was this investigation sufficient, and what did its process teach? |
+| Blindspot review (optional by default) | `inv blindspots` checks recorded structure; `review.submit` with `kind: blindspot` captures reviewed scope, gaps, exact read-set, and explicit `context_isolation` level. | Did the challenge incorporate fresh context, blinded review, or new external checks? Same-session LLM self-critique without external feedback cannot be treated as reliable self-correction. |
+| Close substantive case | Answer/choice/outcome or explicit inconclusive reason; outstanding questions, limitations, reopen triggers; attributed method Retrospective required when case meets materiality criteria (promoted conclusion/decision, failed/invalidated run, or retracted evidence) or pack policy; routine/short cases may record explicit `retrospective_waived` reason; `record`-only sessions exempt. | Was this investigation sufficient, and are any recorded lessons grounded in observed case events rather than boilerplate? |
 
 Do not make every case satisfy every gate. A documentary research case can rely on source criticism rather than an irrelevant control experiment. A performance case can close with a conditional improvement choice despite uncertain root cause. A design choice can be accepted with acknowledged subjective priorities, without claiming empirical truth.
 
@@ -739,10 +739,10 @@
 - A case is repeatedly collecting evidence without any recorded discriminatory effect or decision consequence.
 - A case ready for closure has no substantive Retrospective.
 
-An agent can then request `inv guide blindspot` for a **semantic adversarial review**: what other mechanism, hidden constraint, falsifier or measurement failure might matter? The agent records the result through `review.submit` with `kind: blindspot`, an exact case/conclusion/decision snapshot/read-set, scope inspected, method, identified gaps (including zero with a bounded reason), and optionally new Question/Assumption/Objection records in the same atomic transaction. A review of one narrow claim is not whole-case coverage. A changed material source makes dependent reviews stale; the reviewer must explicitly recheck.
+An agent can then request `inv guide blindspot` for a **semantic adversarial review**: what other mechanism, hidden constraint, falsifier or measurement failure might matter? Because empirical LLM evaluations show intrinsic same-session self-correction without external feedback frequently fails or degrades accuracy, `review.submit` with `kind: blindspot` MUST declare `context_isolation` (`same_session_self_review | fresh_context_unblinded | fresh_context_blinded | external_evidence_backed`), alongside exact case/conclusion/decision snapshot/read-set, scope inspected, method, identified gaps (including zero with a bounded reason), and optionally new Question/Assumption/Objection records in the same atomic transaction. A review of one narrow claim is not whole-case coverage, and `same_session_self_review` is visibly flagged as low-independence in briefs and reports. A changed material source makes dependent reviews stale; the reviewer must explicitly recheck.
 
 `inv next`/`frontier` surface the highest-impact unresolved gaps, but **the checker cannot know all possible explanations**, so “0 detected structural gaps” must never display as “investigation complete.” No default policy requires a second agent or an LLM call; an opt-in method pack may require an attributed challenge review at consequential transitions.
 
@@ -771,7 +771,7 @@
 
 `inv retrospective --case C --format json` reads the current methodological record, showing evidence links and whether proposed lessons were reviewed. `retrospective.record` persists an attributed case-scoped summary of **methods attempted, effective discrimination, wasted/invalid or misleading steps (or explicit none observed), investigation cost/constraints when known, uncertainty that remains, and specific next-time improvements**. It may be updated only by supersession; the earlier retrospective remains readable. A brief and a report show only a bounded highlights section with exact references.
 
-For a substantive `case.close`, require a Retrospective from the same or preceding transaction, bound to a case snapshot that remains current for materially relevant work at closure. If important new evidence or a changed decision basis intervened, require an updated Retrospective rather than accepting an obsolete account. Support one atomic batch creating the retrospective and closing with a `client_ref`, so the rule costs one call; `record`-only sessions are exempt. Do not force an agent to claim failures it did not see. “No reusable lesson identified” is valid with a bounded reason, not a fabricated self-congratulatory success. A failed or inconclusive case can still yield a useful method lesson.
+For a consequential `case.close` (any case with a promoted Conclusion, accepted Decision, failed/invalidated Run, retracted evidence, or pack-declared trigger), require a Retrospective from the same or preceding transaction, bound to a case snapshot that remains current for materially relevant work at closure. Low-complexity or abandoned non-record cases may close with an explicit `retrospective_waived` reason to avoid bureaucratic boilerplate compliance. Every entry in `lessons[]` must reference at least one concrete recorded event/object (e.g., an invalidated Run, revised prediction, retraction, or blindspot gap); ungrounded generic platitudes fail schema validation. If important new evidence or a changed decision basis intervened, require an updated Retrospective rather than accepting an obsolete account. Support one atomic batch creating the retrospective and closing with a `client_ref`, so the rule costs one call; `record`-only sessions are exempt. “No reusable lesson identified” is valid with a bounded reason, not a fabricated self-congratulatory success.
 
 Lessons remain **case-scoped observations about process**, not accepted general rules. Mark proposed reusable lessons with applicability, counterexamples/limits and supporting cases. The default never automatically rewrites method packs, agent instructions, prompts or integration config. A later explicit, reviewed `method-pack` authoring workflow may incorporate them; that is outside v1.
@@ -1512,6 +1512,7 @@
 
 - whether the reviewer saw the original conclusion;
+- `context_isolation`: `same_session_self_review | fresh_context_unblinded | fresh_context_blinded | external_evidence_backed` (distinguishing ungrounded LLM self-reflection from context-isolated or externally checked critique);
 - which exact artifacts and criteria were available;
 - whether new evidence was collected;
 - whether method, environment, fixture, or source changed;
```

---

### [High Impact, Low Effort] Change #4: Pin Safe OS Locking (`flock`/OFD), Non-Clobbering Atomic Publish (`renameat2`), and Float-Free Canonical Hashing (§10.1–§10.2)

**Current State:**  
1. §10.1 specifies *"A short OS advisory lock"* without excluding POSIX `fcntl(F_SETLK)`.
2. §10.2 step 7 states *"atomically publish to its unique final path without clobbering an existing event"* without specifying how, despite POSIX `rename()` silently clobbering existing targets.
3. §10.1 specifies **RFC 8785 (JCS)** canonical JSON hashing across Rust and Python, allowing numeric fields while relying on cross-language JCS equivalence.

**Proposed Change:**  
1. **Explicitly mandate `flock()`** (or Linux `F_OFD_SETLK` open-file-description locks) on `state/locks/commit.lock` and **explicitly forbid POSIX `fcntl(F_SETLK)`** due to its notorious "close any FD drops all locks in the process" semantics.
2. **Specify exact non-clobbering publication** for immutable blobs and segment creation: `renameat2(..., RENAME_NOREPLACE)` on Linux (or `link()` + `unlink()` fallback where unsupported), failing hard on `EEXIST`, followed by `fsync()` on the file and `fsync()` on the parent directory.
3. **Eliminate IEEE-754 float canonicalization hazards in hashed envelopes:** Either (a) **forbid JSON floating-point numbers in canonical hashed transaction envelopes** (encode decimal measurements as exact canonical strings or `{mantissa, scale}` / `{decimal: "18.7"}` structs, restricting JSON numbers in canonical events to safe integers `[-(2^53-1), 2^53-1]`), OR (b) hash the exact stored canonical UTF-8 byte sequence while validating strict I-JSON + no-float rules, AND include RFC 8785 conformance vectors covering `-0.0`, `1e+21`, `1` vs. `1.0`, and UTF-16 surrogate-pair key sorting across Rust and Python.

**Rationale:**  
- **POSIX `fcntl` locking pitfall:** Under POSIX `fcntl(F_SETLK)`, if *any* thread or third-party library opens and closes a second file descriptor to the lock file, the kernel silently releases **every** lock held by the process on that inode. `flock()` and `F_OFD_SETLK` bind locks to the open file description instead.
- **POSIX `rename()` clobbering:** Standard `rename(tmp, dst)` atomically *overwrites* `dst`. Without `renameat2(RENAME_NOREPLACE)` or `link()` + `unlink()`, a lock-bypassing bug or crash recovery race could silently clobber an immutable blob or committed event.
- **RFC 8785 cross-language traps:** Empirical cross-language JCS audits between Rust (`serde_json`/`ryu`) and Python (`json.dumps`) document repeated hash mismatches on `-0.0` vs. `0`, ECMAScript positive exponent formatting (`1e+21` vs. `1e21`), integer-vs-float (`1` vs. `1.0`), and UTF-16 code-unit key ordering vs. UTF-8 byte ordering. Disallowing raw JSON floats in canonical hashed event envelopes (storing measured decimals as exact string representations like `"18.7"`) removes the entire class of cross-language float-formatting bugs while also preserving measurement significant digits (`"18.70"` vs. `"18.7"` — a scientific bonus!).

**Benefits:**
- Prevents silent lock drops in multi-threaded Rust binaries/libraries (`investigation-core` embedded in external apps).
- Prevents accidental clobbering of immutable blobs/segments.
- Eliminates Rust-vs-Python JCS hash divergence AND preserves exact measurement significant figures (`"18.70"` vs. `"18.7"`).

**Trade-offs:**
- Measurements in JSON payloads use exact decimal strings (e.g., `"value": {"decimal": "18.70", "unit": "ms"}`) instead of raw JSON floats, requiring decimal parsing when numeric comparisons are performed.
- `renameat2(RENAME_NOREPLACE)` is Linux-specific (macOS requires `renamex_np(RENAME_EXCL)`, Windows requires `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`), matching the plan's Linux-first qualification rule.

**Implementation Notes:**
- In Rust, prefer `rustix::fs::flock` / `fs2` (verified to use `flock` on Unix and `LockFileEx` on Windows, never `fcntl(F_SETLK)`).
- Add cross-language canonicalization fixtures (Rust + Python) explicitly testing non-ASCII supplementary-plane keys, safe-integer boundaries (`2^53 - 1`), duplicate-key rejection, and rejection of raw JSON floats in canonical envelopes.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1063,7 +1063,7 @@
 
 ### 10.1 Canonical journal
 
-Use one linear workspace sequence for v1. A short OS advisory lock serializes commits across local processes. This makes cross-record transactions straightforward; case reads and file ingestion need not hold the lock while doing expensive work.
+Use one linear workspace sequence for v1. A short OS advisory lock using **open-file-description semantics (`flock` on Unix / `F_OFD_SETLK` on Linux / `LockFileEx` on Windows; NEVER process-scoped POSIX `fcntl(F_SETLK)`, which silently releases all locks when any FD to the inode is closed)** serializes commits across local processes and threads. This makes cross-record transactions straightforward; case reads and file ingestion need not hold the lock while doing expensive work.
 
 Each immutable transaction file contains:
 
@@ -1077,7 +1077,7 @@
 
 Original imported manifests and finalized run manifests are immutable Artifact bytes referenced by journal events. Convenience manifest files under `cache/imports/` are rebuildable copies, never indispensable loose provenance. A finalization manifest lists its covered artifacts and pre-finalization snapshot; it excludes itself and its subsequently committed transaction hash to avoid recursive hashing. The finalization event binds that manifest's digest to the Run revision.
 
-Use SHA-256 for artifact identity and transaction integrity. Define transaction hashing as SHA-256 over a versioned domain-separation prefix plus RFC 8785 canonical JSON of the envelope excluding `transaction_hash`. UTF-8 only; reject duplicate JSON keys, non-finite values, and integers outside the interoperable safe range. Represent exact large counters/timestamps/decimals as documented strings where needed. Do not normalize or rewrite artifact bytes.
+Use SHA-256 for artifact identity and transaction integrity. Define transaction hashing as SHA-256 over a versioned domain-separation prefix plus RFC 8785 canonical JSON of the envelope excluding `transaction_hash`. UTF-8 only; reject duplicate JSON keys, non-finite values, and integers outside the IEEE-754 safe range `[-(2^53-1), 2^53-1]`. **Prohibit raw IEEE-754 floating-point numbers inside canonical hashed transaction envelopes:** represent all decimal measurements, large counters, and timestamps as canonical exact strings (e.g., `{"decimal": "18.70", "unit": "ms"}`), which eliminates Rust-vs-Python RFC 8785 float-formatting divergence (`-0.0`, `1e+21`, `1` vs. `1.0`) and preserves scientific significant figures. Test UTF-16 code-unit key sorting against official RFC 8785 vectors across Rust and Python. Do not normalize or rewrite artifact bytes.
 
 Keep canonicalization fixtures for Rust and any future reader. The plugin does not need to write canonical journal files; it submits commands to the core.
 
@@ -1088,8 +1088,8 @@
 3. Discover the last committed transaction. Bring the in-memory/SQLite state through that sequence, verifying the tail. Never validate against a stale index.
 4. Look up `(workspace_id, actor, command_id)` in canonical-derived state. Identical request: return the original receipt. Different request: reject.
 5. Validate entity revisions, required read-set, references, mode-specific obligations, and every operation against a temporary state. Assign actual IDs once.
-6. Publish needed immutable blobs with exclusive-create semantics; verify any existing digest path. Flush bytes/directories as supported. Missing staged blobs prevent commit.
-7. Write the entire transaction to a temporary file on the same filesystem, flush it, atomically publish to its unique final path without clobbering an existing event, and flush the directory.
+6. Publish needed immutable blobs on the same filesystem, `fsync()` the temp file, and publish with non-clobbering semantics (`renameat2(..., RENAME_NOREPLACE)` on Linux, or `link()`+`unlink()` fallback—never plain POSIX `rename()`, which silently overwrites existing files); on `EEXIST`, verify existing bytes match the digest before proceeding. `fsync()` the parent directory. Missing staged blobs prevent commit.
+7. Persist the transaction durably (see segmented append-only WAL in §10.1 or non-clobbering `renameat2(RENAME_NOREPLACE)` on segment creation), calling `fsync()` on the data file and—whenever a new file is created—`fsync()` on the parent directory.
 8. This successful durable publication is the commit boundary. Update the disposable index in one SQLite transaction and record its applied sequence/head.
 9. Release the commit lock. Render derived views synchronously or mark them pending according to configured mode.
 10. Return a receipt distinguishing canonical commit, index health, and vault freshness.
```

---

### [High Impact, High Effort] Change #5: Upgrade Causal & Experimental Methodology with Severe Testing (Mayo), Causal Factor Roles / Collider Warnings (Pearl), and Strong Inference Diagnosticity (Platt) (§2.1, §2.4, §5.2, §6.4, §8.5)

**Current State:**  
The plan models hypothesis testing through `InquiryPlan` predictions and qualitative `Assessment` relations (`supports`, `contradicts`, `non_diagnostic`, `inconclusive`), citing ACH and generic "confounds/controls." It lacks:
1. Any representation of **test severity / diagnosticity** (whether an outcome would also be expected under rival hypotheses).
2. Any structural distinction between **confounders** ($X \leftarrow C \to Y$), **mediators** ($X \to M \to Y$), and **colliders / selection filters** ($X \to Z \leftarrow Y$).

**Proposed Change:**  
1. **Embed Deborah Mayo's Severe Testing & John Platt's Strong Inference into `InquiryPlan` and `Assessment`:**
   - For confirmatory `InquiryPlan` objects, require a **rival prediction table** (`predictions[]` mapping each active rival hypothesis to its expected observable outcome under the test), making non-discriminating tests deterministically detectable *before* execution.
   - On `Assessment` records claiming `supports`, capture an explicit **severity / diagnosticity rationale**: *"If this hypothesis were false (and a rival or instrument artifact were responsible), how likely would this test have failed to detect the error?"*
2. **Add typed causal factor roles (Judea Pearl's DAG principles):**
   - Allow `InquiryPlan`, `Observation`, and `Assumption` records to tag controlled/filtered variables with `causal_role: confounder | mediator | collider_selection_filter | nuisance_factor | unknown`.
   - Add two deterministic `inv blindspots` checks:
     - `inquiry.non_discriminating_plan`: flags a confirmatory plan where ≥2 active rival hypotheses have identical predicted outcomes.
     - `causal.failure_only_selection`: flags an explanatory Conclusion whose supporting observations all derive from failure-filtered/crash-only sources with zero baseline/non-failure observations (classic collider/selection bias).
3. **Add a `causal-structure` guidance card** in §2.4 teaching confounders vs. mediators vs. colliders and severe testing.

**Rationale:**  
- **Deborah Mayo's Severe Testing principle** (*Statistical Inference as Severe Testing*, 2018) establishes that passing a test only constitutes positive evidence for $H$ to the extent that the test had a high probability of **exposing $H$ as false if $H$ were false**. Without recording counterfactual predictions for rival hypotheses (Platt's *Strong Inference*, 1964), agents commit classic confirmation bias: running tests that would pass under *both* $H_1$ and $H_2$ and logging `supports(H_1)`.
- **Judea Pearl's Causal Hierarchy** shows that "controlling for everything" is actively harmful: controlling a **confounder** blocks bias, controlling a **mediator** erases a real causal effect, and conditioning/filtering on a **collider** (e.g., inspecting *only* crashed requests or *only* slow queries!) *manufactures* spurious correlations between independent causes. In software debugging and performance analysis, failure-only log filtering is pervasive and almost never caught by generic "check confounds" prose.
- Empirical evaluations (*Dhami et al., 2019; Mandel et al., 2018*) showed qualitative ACH matrices without strict diagnosticity discipline fail to debias analysts; making rival-outcome discrimination and severity structurally checkable upgrades ACH from a passive table into a rigorous error-probing workflow.

**Benefits:**
- Deterministically catches non-discriminating experiments *before* external resources are spent running them.
- Catches the single most common causal fallacy in debugging/incident analysis (conditioning on failure-only traces without healthy-run baselines).
- Elevates the investigative methodology to contemporary philosophy-of-science and causal-inference standards without requiring heavy formal math or Bayesian priors.

**Trade-offs:**
- Slightly richer schema for confirmatory `InquiryPlan` (`predictions[]` keyed by rival hypothesis refs) and optional `causal_role` annotations.
- Exploratory cases remain completely exempt (preserving cheap capture).

**Implementation Notes:**
- Keep causal factor roles lightweight (enum tags + brief note, not a full graphical `do`-calculus solver).
- `inquiry.non_discriminating_plan` can compare normalized outcome categories (e.g., `expected_direction: increase | decrease | no_change | error | distinct_signature`) across rivals to deterministically flag identical predictions while still allowing free-text nuance.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -132,7 +132,7 @@
 | Research method | General principle | Product requirement | References |
 |---|---|---|---|
-| Scientific/hypothetico-deductive troubleshooting | Iterate between measured behavior, explanations, tests and causal intervention. | Capture baseline, competing root causes, test setup, valid/invalid outcomes and reproduction limits. | [G1], [G2] |
+| Strong Inference, Severe Testing & Causal Structure (Platt, Mayo, Pearl) | Devise crucial tests that cleanly separate rival hypotheses; data supports H only if the test would likely have exposed H as false; distinguish confounders (control), mediators (do not block), and colliders (do not condition/filter on joint effects). | Rival-prediction tables on confirmatory plans, counterfactual test-severity rationale on supporting assessments, typed causal-factor roles, and failure-only selection blindspot checks. | [G1], [G2] |
 | Delta debugging / differential testing | Isolate minimal difference between passing and failing conditions. | Support paired runs, fixed environment descriptions, reduction trials and source-linked claims. | [G3] |
 | Experimental controls and replication | Distinguish noise, confounds and invalid instruments from meaningful effects. | Immutable measurement conditions, controls, run groups, source independence and honest negative results. | [M10], [M11], [G4] |
@@ -186,6 +186,7 @@
 | `measure` | Quantitative observation | Units, workload/sample, environment, instrument, uncertainty, capture window. |
 | `baseline` | Regression/comparison | Comparable baseline and confound checklist. |
 | `discriminate` | Competing explanations | A test whose results distinguish hypotheses, with predicted outcomes. |
+| `causal-structure` | Causal claim or filtered dataset | Distinguish confounders (control), mediators (preserve), and colliders/selection filters (do not analyze failure-only logs without healthy baselines); evaluate test severity (would this test have caught H if H were false?). |
 | `negative-result` | Absence/non-effect | Observation opportunity, detection threshold, valid controls, bounds. |
 | `delta-debug` | Reproducible failure with many changed factors | Minimal reproducible difference and limitations of reduction. |
@@ -375,9 +376,9 @@
 | **Source** | Origin and collection context | Producer/publisher, acquisition, identity, time, access, verification limits. |
 | **Artifact** | Bytes or explicit external reference | Digest, size, format, source, transforms, availability and sensitivity. |
 | **Observation** | Bounded measured/reported datum | Value/statement/units, source selector, extraction method, quality, limitations. |
-| **InquiryPlan** | Planned inspection, experiment, benchmark or source check | Purpose, method, predictions if appropriate, controls, limits, desired observations. |
+| **InquiryPlan** | Planned inspection, experiment, benchmark or source check | Purpose, method, rival-hypothesis prediction table (for confirmatory mode), controlled/excluded factors with optional `causal_role` (`confounder|mediator|collider_selection_filter|nuisance_factor`), limits, desired observations. |
 | **Run** | One recorded attempt/execution/inspection | Exact plan ref, producer, tool/environment, timings, result assertion, artifacts, execution status. |
-| **Assessment** | Attributed interpretation of evidence about an object | Subject+revision, evidence refs, relation, rationale, assumptions, uncertainty, basis digest. |
+| **Assessment** | Attributed interpretation of evidence about an object | Subject+revision, evidence refs, relation, rationale, counterfactual test-severity / diagnosticity note (for `supports`), assumptions, certainty/likelihood, basis digest. |
 | **Conclusion** | Bounded synthesized claim about what was learned | Statement, significance, scope, support/contradictions, review and effective disposition. |
@@ -536,7 +537,7 @@
 | Record observation | Source selector or explicitly labeled assertion, extraction/measurement metadata. | Is statement genuinely observed rather than inferred? |
 | Freeze exploratory InquiryPlan | Purpose, method or inspection, observation channel and bounded stop rule. | Is exploration useful? |
-| Freeze confirmatory InquiryPlan | Exact question/hypothesis revisions, predicted results, observation and control conditions, boundaries. | Does the proposed test actually discriminate? |
+| Freeze confirmatory InquiryPlan | Exact question/hypothesis revisions, **per-rival predicted outcomes** (Platt discrimination check), observation/control/selection conditions with causal roles, boundaries. | Does the proposed test severely probe H and avoid conditioning on a collider or mediator? |
 | Record Run | Producer identity, plan or exploratory purpose, external reference, explicit status, honest completeness. | Were instruments valid? Was run comparable? |
@@ -732,6 +733,8 @@
 
 - A causal Conclusion lacks an acknowledged rival account or explicit reason alternatives were not useful.
 - Confirmatory empirical claims rely on invalid/missing control, baseline, instrument, sample/window or frozen-plan basis.
+- A confirmatory InquiryPlan predicts indistinguishable outcomes across active rival hypotheses (`inquiry.non_discriminating_plan`), or a supporting Assessment lacks a counterfactual test-severity rationale.
+- An explanatory Conclusion relies solely on failure-filtered/crash-only observations with no non-failure baseline (`causal.failure_only_selection`, collider/selection bias risk).
 - Multiple citations masquerade as independent evidence despite common original provenance.
 - A material Assumption, critical Objection, stale source or retraction still affects a promoted record.
```

---

### [High Impact, High Effort] Change #6: Upgrade Decision Analysis with Weight-Free Pareto Dominance, Value-of-Information Triage, Premortems (Klein), and Reference Class / Base Rates (Flyvbjerg) (§2.1, §2.4, §7.2, §15.1)

**Current State:**  
§15.1 properly rejects opaque weighted-sum scoring and supports hard/soft criteria, comparisons, and prose sensitivity notes. However, it provides no deterministic way to prune dominated options, no structural connection between missing cells and decision-flipping **Value of Information (VoI)**, no **Premortem** workflow to uncover failure mechanisms before committing, and no **Outside-View Reference Class** grounding for engineering/cost/risk estimates.

**Proposed Change:**  
1. **Deterministic weight-free Pareto frontier analysis (§15.1):** Since every `Criterion` already declares `preferred_direction` and `hard|soft` (§9.4), compute and expose each eligible option's **Pareto status** deterministically without inventing weights:
   - `disqualified_hard`: fails a verified hard criterion.
   - `strictly_dominated`: another eligible option is at least as good on every comparable criterion (with no `unknown` cells that could reverse it) and strictly better on at least one.
   - `pareto_frontier`: undominated given current evidence.
   - `uncertain_frontier`: dominance cannot be determined due to `unknown` cells on deciding criteria.
2. **Connect `inv next` / `inv frontier` to qualitative Value of Information (VoI) (§7.2):** Prioritize measuring `unknown` option × criterion cells *only* when resolving them could plausibly alter the Pareto frontier or a hard-criterion disposition (`voi_status: could_flip_frontier | hard_gate_unresolved | dominated_low_voi`). Stop wasting agent turns benchmarking a soft criterion on an option already disqualified by a hard constraint or strictly Pareto-dominated!
3. **Add `premortem` and `reference-class` to evaluative workflows (§2.4, §15.1):**
   - `premortem` guidance card + optional `premortem_scenarios[]` on `decision.propose` (assuming the chosen option failed 6–12 months post-adoption and tracing concrete failure mechanisms back to today's assumptions and reopen triggers).
   - `reference-class` guidance card prompting an outside-view base-rate anchor (historical data from comparable systems/migrations/teams) before accepting optimistic inside-view cost, latency, or reliability projections.

**Rationale:**  
- **Pareto dominance** is mathematically rigorous, completely weight-free, and directly solves the plan's tension between "no arbitrary weighted sums" and "how does the tool actually help narrow 6 options down to 2?"
- **Value of Information (VoI)** (*Howard, 1966; Raiffa*) is the core theorem of rational inquiry: information whose resolution cannot change the chosen action has **zero decision value**. Tying `inv next` to Pareto/hard-constraint frontier impact turns §7.2's manual `could_flip_choice` tag into a deterministic, high-leverage pruning engine.
- **Premortems (Gary Klein, 2007)** use prospective hindsight to increase identified failure reasons by ~30% and break anchoring/optimism bias far more effectively than asking "what are the tradeoffs?"
- **Reference Class Forecasting (Kahneman & Flyvbjerg)** is the primary empirically validated antidote to the planning fallacy in engineering/architecture trade studies.

**Benefits:**
- Gives evaluative investigations genuine analytical power without ever fabricating composite scores.
- Eliminates wasted agent work gathering data on hard-failed or Pareto-dominated options.
- Produces dramatically stronger `decision-record` ADRs with concrete premortem-derived reopen triggers and outside-view sanity checks.

**Trade-offs:**
- Strict Pareto dominance requires explicit ordinal/numeric comparability semantics on criteria; incomparable/subjective criteria must conservatively remain `incomparable` so the core never falsely declares dominance.

**Implementation Notes:**
- Conservative dominance rule: Option A strictly dominates Option B only if (a) A satisfies all hard criteria B satisfies, (b) for every active comparable criterion, A is strictly or weakly better than B with valid/current evidence (no `unknown` or `stale` cell where B might beat A), and (c) A is strictly better on at least one criterion. If any criterion is `subjective_unranked` or `unknown`, mark dominance `indeterminate` rather than falsely pruning.
- Surface `pareto_status` in `inv brief`, `inv frontier`, and the generated Obsidian comparison matrix.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -136,7 +136,7 @@
 | Experimental controls and replication | Distinguish noise, confounds and invalid instruments from meaningful effects. | Immutable measurement conditions, controls, run groups, source independence and honest negative results. | [M10], [M11], [G4] |
 | Analysis of competing hypotheses / intelligence methods | Explicit alternatives, linchpin assumptions and diagnostic evidence. | Qualitative relationship matrix, contradictions, scoped standing, no evidence-vote score. | [M1]–[M5] |
-| Decision analysis / trade studies | Specify must-haves, criteria, option performance, sensitivity and uncertainty before choosing. | First-class options, criteria, versioned measurements and criterion assessments; conditional decisions. | [G5], [G6] |
+| Decision analysis, Pareto pruning, Premortems & Reference Classes | Specify must-haves, prune strictly dominated options without invented weights, prioritize information by Value of Information (VoI), anchor estimates to outside-view base rates, and run prospective-hindsight premortems. | First-class options/criteria/comparisons, deterministic weight-free Pareto status, VoI-driven frontier prioritization, optional premortem failure scenarios and reference-class anchors, conditional decisions. | [G5], [G6] |
 | Architecture decision records | Persist context, decision and consequences; supersede rather than rewrite accepted history. | Immutable accepted decisions and exportable ADR-style reports. | [G7], [G8] |
@@ -191,6 +191,8 @@
 | `criteria` | Choosing among options | Explicit must-haves, priorities, measurable/subjective criteria. |
 | `compare` | Option evaluation | Per-option/per-criterion evidence, uncertainty and missing cells. |
 | `sensitivity` | Close or uncertain choice | Which assumption/weight/measurement could reverse preference? |
+| `premortem` | Before proposing/accepting a consequential decision | Assume the chosen option failed catastrophically 6–12 months later; trace specific failure histories back to untested assumptions and concrete reopen triggers. |
+| `reference-class` | Inside-view cost, effort, or reliability projection | Anchor projections against historical base rates from comparable systems/projects before relying on bespoke estimates. |
 | `independence` | Apparent corroboration | Shared sources/acquisition/measurement dependencies. |
@@ -603,7 +605,7 @@
 1. Canonical store integrity or invalid observation blockers.
 2. Time-sensitive preservation explicitly declared by investigator.
 3. Contradictions or missing hard requirements affecting a consequential conclusion/decision.
-4. Declared decision-changing value (`could_flip_choice`, `could_discriminate`, `context_only`, `unknown`).
+4. Decision-changing Value of Information (VoI): prioritize unresolved cells/assumptions that can flip a hard constraint or Pareto-frontier standing (`could_flip_choice`, `could_discriminate`), and explicitly deprioritize measuring soft criteria on already hard-disqualified or strictly Pareto-dominated options.
 5. Applicability/relevance to the current question and decision deadline.
 6. Bounded cost/effort supplied by the user, if comparable.
 7. Novelty compared to previous failed/inconclusive attempts.
@@ -1546,9 +1548,11 @@
 2. Generate/evidence **Options**; include `do nothing / retain current design` when relevant. Track discarded options and why.
 3. Define **Criteria** *before* reviewing outcomes when possible: hard must-haves, ordinal/quantitative measures, preferred direction, actual units, threshold and relative priority justification. Record any later criterion revision openly.
 4. For each option/criterion cell, attach original evidence, measurement assumptions, observation quality and a bounded conclusion. `unknown` is an expected outcome.
-5. Make consequential uncertainty visible: which missing measurement, preference weight, threshold, or model assumption could reverse the decision? Ask for new evidence only when plausible to matter.
-6. Propose/accept a **Decision** with rationale, rejected viable alternatives, tradeoffs, consequences and reopening trigger. It may be rational but conditional.
+5. Compute **weight-free Pareto status** deterministically across eligible options (`disqualified_hard`, `strictly_dominated`, `pareto_frontier`, `uncertain_frontier`, conservatively treating incomparable/unknown cells as non-dominating). Apply **Value of Information (VoI)** discipline: gather new measurements only for unknowns that could plausibly alter hard-criterion eligibility or the Pareto frontier.
+6. Anchor optimistic inside-view estimates (cost, latency, operational effort) against an outside-view **reference class** where available, and run a **premortem** on the leading option to surface concrete failure mechanisms and actionable reopening triggers.
+7. Propose/accept a **Decision** with rationale, rejected viable alternatives, tradeoffs, premortem risks, consequences and reopening trigger. It may be rational but conditional.
 
 **Do not rank everything with uncalibrated numerical weights.** A weighted score is optional user-provided analytic artifact, not the core's truth criterion. If it is used, preserve all input weights, units, normalization assumptions, sensitivity and why its score should matter. A hard constraint violation cannot be quietly compensated by a weighted soft criterion. For subjective criteria, state preference rather than claiming a measurement.
```

---

### [High Impact, High Effort] Change #7: Add PRISMA-Inspired Search Provenance & Negative-Search Coverage to Combat Selection/Reporting Bias (§5.2, §8.5, §17.4)

**Current State:**  
The plan models experimental negative results (`negative-result` card, §6.4 "Claim an informative absence"), but documentary, literature, and codebase investigations have no structured representation of **what corpora/repositories were searched, with what queries/filters, and what returned zero or excluded hits**.

**Proposed Change:**  
1. Explicitly support **documentary/code search Runs** via `InquiryPlan` (`method: systematic_search`) and `Run` records capturing: `corpus_or_scope`, `queries_or_filters[]`, `date_or_revision_bounds`, `inclusion_exclusion_criteria`, `hits_total`, `hits_examined`, and `negative_or_empty_searches[]`.
2. Add a deterministic `inv blindspots` rule (`source.absence_without_search_log`) that flags any Conclusion or Assessment asserting non-existence in a corpus (e.g., *"no errata exist," "no other callers found," "no rival vendor offers X"*) without a linked search Run documenting the scope and queries checked.
3. Update Scenario D (§17.4) to exercise negative-search provenance (verifying whether a newer first-party erratum was actually searched for rather than merely absent from ingested files).

**Rationale:**  
In systematic review methodology (**PRISMA 2020**; Cochrane Risk of Bias), **selection and reporting bias** (the file-drawer problem) is just as fatal to validity as instrument error in a lab experiment. When an LLM agent investigates a codebase, specification, or market landscape, it naturally ingests only the documents it *found* and liked. A second agent or human reviewer cannot distinguish:
- *"We searched the official errata registry and commit log through 2026-11-01 and confirmed zero matching notices"*, from
- *"We only read two blog posts and never checked the errata registry."*
Logging search scope and negative hits turns documentary absence from an invisible blindspot into auditable provenance.

**Benefits:**
- Prevents resuming agents from repeating identical fruitless searches (directly fulfilling Read-This-First Goal #6).
- Closes the reporting-bias loophole in research, security-review, and codebase-debugging cases.
- Reuses existing `InquiryPlan`/`Run` primitives—no new top-level entity type required.

**Trade-offs:**
- Agents doing deep source research record one extra lightweight `Run` describing their search strategy when making negative/exhaustive claims (optional for ordinary positive citations).

**Implementation Notes:**
- Keep this inside the existing `InquiryPlan` + `Run` schema via a structured optional `search_coverage` block rather than adding a 19th entity type.
- Expose a compact `Search Log` table in the Obsidian `Sources` view for `research`-mode cases.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -378,7 +378,7 @@
 | **Observation** | Bounded measured/reported datum | Value/statement/units, source selector, extraction method, quality, limitations. |
 | **InquiryPlan** | Planned inspection, experiment, benchmark or source check | Purpose, method, predictions if appropriate, controls, limits, desired observations. |
-| **Run** | One recorded attempt/execution/inspection | Exact plan ref, producer, tool/environment, timings, result assertion, artifacts, execution status. |
+| **Run** | One recorded attempt/execution/inspection (including documentary/code searches) | Exact plan ref, producer, tool/environment, timings, result assertion, artifacts, execution status, and optional PRISMA-style `search_coverage` (corpus/revision bounds, queries, inclusion/exclusion rules, total/examined/empty hits). |
 | **Assessment** | Attributed interpretation of evidence about an object | Subject+revision, evidence refs, relation, rationale, assumptions, uncertainty, basis digest. |
@@ -736,6 +736,7 @@
 - A material Assumption, critical Objection, stale source or retraction still affects a promoted record.
 - An evaluative Decision has missing/unknown hard-criterion cells, inconsistent units, undisclosed subjective preferences, or no reopening trigger.
 - An important claimed fact has no retained/external source selector or producer attribution.
+- An informative-absence claim in documentary/code research lacks a recorded search Run specifying corpus bounds, queries, and empty/excluded results (`source.absence_without_search_log`, combating PRISMA reporting/selection bias).
 - A case is repeatedly collecting evidence without any recorded discriminatory effect or decision consequence.
 - A case ready for closure has no substantive Retrospective.
@@ -1688,7 +1689,7 @@
 
 Seed a primary document, a trade article quoting it, a blog summarizing the article, and a newer first-party erratum. The agent must identify that the blog and article share *one original acquisition*, record the erratum's effect on currentness, and form a conditional conclusion (or explicit insufficient-evidence result) with direct source links.
 
-**Review/learning step:** conduct a blindspot review of source dependence, currency and missing first-party documentation; produce an explicit Retrospective stating why multiple downstream reports did not add independent support.
+**Review/learning step:** record a bounded `search_coverage` Run documenting which first-party repositories/errata registries were queried (and any empty searches) so reporting bias is visible; conduct a blindspot review of source dependence, currency and missing first-party documentation; produce an explicit Retrospective stating why multiple downstream reports did not add independent support.
```

---

### [High Impact, High Effort] Change #8: Replace One-File-Per-Event Journal with a Segmented Append-Only Framed Log (§4.2, §10.1, §10.2, §10.4)

**Current State:**  
§4.2 (`state/events/000000000001-<sha256>.json`) and §10.1–§10.2 store every committed transaction as a **separate individual JSON file** in a single flat directory, requiring per-event temp file creation, `fsync()` on the file, atomic publish, and `fsync()` on the parent directory for every single write—while §10.5 targets `<150 ms p95` mutations at 10,000+ objects and 50,000+ relations.

**Proposed Change:**  
Replace one-file-per-event with a **segmented append-only framed log** (`state/journal/seg-00000001.jsonl` or length-prefixed JSONL frames with explicit frame length + SHA-256 chain + trailing newline):
1. Append each committed transaction sequentially to the active segment file and call `fsync()` on the segment FD (zero per-commit directory metadata mutations or parent-dir `fsync()`).
2. Roll over to a new segment at a configurable size cap (e.g., 16 MiB), calling `fsync()` on the parent directory **only** on segment rollover (once per ~10,000 events instead of on every event), and sealing closed segments as immutable (`0444`).
3. On startup/recovery, verify the tail frame of the active segment: if a crash left an incomplete trailing write (length/checksum mismatch at EOF), diagnose it explicitly and either truncate the uncommitted partial tail under the commit lock (logging a recovery diagnostic) or halt if corruption occurs mid-segment.
4. Retain content-addressed individual files for `state/blobs/sha256/<prefix>/<digest>` (which are sharded and less frequent).

**Rationale:**  
Benchmarking and kernel analysis of Linux `ext4`/`xfs` durability consistently show that **one-file-per-event with directory `fsync()`** is an anti-pattern for event-sourced stores:
- Every single commit forces inode allocation, HTree directory modification, and a synchronous `jbd2` journal commit on the parent directory—causing severe latency spikes and SSD write amplification, especially under `capture append` bursts or multi-case workspaces.
- Replaying 20,000 events requires 20,000 `open()`/`read()`/`close()` syscalls plus `opendir()`/`readdir()` sorting instead of 1–2 sequential `read()` streams hitting kernel page-cache read-ahead.
- Backups (`workspace-archive`) and filesystem inode quotas degrade dramatically with tens of thousands of tiny ~500-byte files.
- A segmented append-only framed JSONL log preserves **every single human-inspectability, immutability, hash-chaining, and zero-external-DB benefit** of JSON event files while delivering 10×–50× better durability throughput and replay speed.

**Benefits:**
- Eliminates per-commit directory `fsync()` bottlenecks and inode exhaustion; easily clears the `<150 ms p95` write target even on consumer SSDs.
- Fast sequential replay when rebuilding `cache/index.sqlite`.
- Remains 100% plain-text UTF-8 JSON per line—trivially inspectable with standard CLI tools (`jq`, `rg`, `tail`).

**Trade-offs:**
- Crash recovery must carefully handle an uncommitted partial write at the tail of the active segment (well-understood WAL framing pattern: record byte-length header + canonical JSON + SHA-256 hash + `\n`; any incomplete trailing bytes past the last valid framed record are uncommitted, never partially applied).
- Mid-file corruption checks still scan sequentially per segment (fast, and accelerated by checkpoints).

**Implementation Notes:**
- Frame format suggestion: `<byte_len_decimal> <canonical_json_with_transaction_hash>\n` (or NDJSON with embedded `transaction_hash` verified over the line bytes excluding the hash field). Using a length prefix makes partial-tail detection deterministic even if a write stops mid-UTF-8 sequence.
- Use `O_APPEND` + `flock` commit lock; record `last_valid_offset` in SQLite metadata so warm tail verification reads only newly appended bytes.
- Add crash-injection tests in §20.2 specifically covering torn writes mid-frame, mid-newline, and during segment rollover.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -285,7 +285,7 @@
   investigation.toml
   state/                             # Canonical; NOT the Obsidian vault
     workspace.json
-    events/000000000001-<sha256>.json
+    journal/seg-00000001.jlog        # Segmented append-only framed canonical JSON log
     blobs/sha256/<prefix>/<digest>
     checkpoints/<checkpoint-id>.json
     staging/<run-id>/               # Durable interrupted-producer recovery
@@ -1065,7 +1065,7 @@
 
 Use one linear workspace sequence for v1. A short OS advisory lock serializes commits across local processes. This makes cross-record transactions straightforward; case reads and file ingestion need not hold the lock while doing expensive work.
 
-Each immutable transaction file contains:
+Store transactions in **segmented append-only framed log files** (`state/journal/seg-XXXXXXXX.jlog`, rolling over at e.g. 16 MiB into sealed read-only segments) rather than one tiny file per event, avoiding ext4 directory-inode `fsync` thrashing and slow `readdir` replays. Each length-prefixed UTF-8 canonical JSON frame contains:
 
 ```text
 format_version, workspace_id, transaction_id, sequence, previous_hash,
@@ -1073,7 +1073,7 @@
 accepted_operations, resolved_ids, resulting_revisions, transaction_hash
 ```
 
-The first transaction follows an immutable genesis record identifying the workspace and schema family. No two committed files may share one sequence. Duplicate sequences, gaps, unexpected parents, unknown required event versions, and digest mismatch are explicit integrity failures, never sorted away.
+The first transaction follows an immutable genesis frame identifying the workspace and schema family. No two committed frames may share one sequence. Duplicate sequences, gaps, unexpected parents, mid-segment truncation, unknown required event versions, and digest mismatch are explicit integrity failures, never sorted away. An incomplete torn frame strictly at EOF of the active segment is diagnosed as an uncommitted crash tail and cleanly recovered under the commit lock.
@@ -1090,7 +1090,7 @@
 5. Validate entity revisions, required read-set, references, mode-specific obligations, and every operation against a temporary state. Assign actual IDs once.
 6. Publish needed immutable blobs with exclusive-create semantics; verify any existing digest path. Flush bytes/directories as supported. Missing staged blobs prevent commit.
-7. Write the entire transaction to a temporary file on the same filesystem, flush it, atomically publish to its unique final path without clobbering an existing event, and flush the directory.
+7. Append the complete length-prefixed transaction frame to the active journal segment and `fsync()` the segment file descriptor. When rolling over to a new segment, create it with `O_EXCL`, seal the prior segment read-only, and `fsync()` the parent directory.
 8. This successful durable publication is the commit boundary. Update the disposable index in one SQLite transaction and record its applied sequence/head.
@@ -1112,8 +1112,8 @@
 | Before blob publication | None | Remove orphan temporary files after an explicit grace period. |
 | After blob publication, before event | Unreferenced immutable blob | Retry same command; reuse bytes. Conservative GC may reclaim only unreferenced blobs after retention policy. |
-| During temp event write | No committed event | Ignore temp; retry same command. |
-| After event publication but uncertain directory flush | Unknown durability to caller | Verify on restart; same command ID resolves ambiguity. Do not create a new command ID. |
+| During partial segment frame append | Torn uncommitted tail at EOF | Detect invalid trailing frame length/hash; safely truncate only the uncommitted tail after last valid frame under commit lock and log recovery event; retry same command ID. |
+| After segment append but uncertain `fsync` | Unknown durability to caller | Verify tail on restart; same command ID resolves ambiguity. Do not create a new command ID. |
 | After durable event, before index | Transaction committed | Replay tail; recover original receipt. |
```

---

### [High Impact, High Effort] Change #9: Eliminate MCP Tool-Schema Bloat with a Compact JIT-Discovery Default Profile (§9.7)

**Current State:**  
§9.7 specifies exposing 10 read tools (`onboard`, `brief`, `next`, `frontier`, `blindspots`, `retrospective`, `get`, `search`, `check`, `schema`) plus ~10 grouped write tools (`capture`, `observations`, `hypotheses`, `inquiries`, `comparisons`, `assessments`, `conclusions`, `decisions`, `reviews`, `retrospectives`) plus `record_batch`, where each write group carries a closed union JSON Schema generated from the ~50-operation catalog.

**Proposed Change:**  
Invert the MCP profile architecture:
1. Make the **default `inv mcp` core profile** a **compact, flat-schema, JIT-discovery surface of ≤7 tools** with a hard manifest budget of **< 3,000 tokens**:
   - `inv_onboard`: workspace/case discovery and workflow guidance.
   - `inv_read`: unified flat-argument read tool (`action: brief | show | explain | search | next | frontier | blindspots | retrospective | check`, with `depth`, `case_id`, `id`, `query`, `max_bytes`).
   - `inv_evidence_read`: explicit opt-in bounded artifact excerpt reader with untrusted-data framing.
   - `inv_schema`: JIT operation catalog + minimal valid example fetcher (`op` or `list`).
   - `inv_record`: single validated atomic transaction executor (matching CLI `inv record`, referencing schemas fetched via `inv_schema`).
   - `inv_ingest`: local source/artifact ingestion.
2. Expose the ~20-tool domain-grouped typed surface only via an explicit `--profile expanded-typed` flag for hosts that natively support deferred/lazy tool loading (`tool_search`).
3. Prohibit deeply nested `oneOf`/`anyOf` discriminated unions in the default MCP tool schemas to ensure compatibility with strict structured-output validators across LLM providers.

**Rationale:**  
Recent 2025–2026 empirical measurements of MCP deployments (Anthropic, Atlassian `mcp-compressor`, Blocks) reveal that eager loading of 20+ richly typed union-schema tools injects **15,000–55,000 tokens** into every single turn before the agent even reads the user's question. This directly sabotages `plan.md`'s own **context-budget discipline** (§8.3) and causes measurable **tool-selection degradation** ("lost in the middle" confusion between overlapping tools like `assessments` vs. `conclusions` vs. `reviews` vs. `record_batch`). Furthermore, several major LLM APIs reject or mis-generate complex nested `oneOf` tool schemas. Since `inv` *already* has a first-class JIT schema discovery command (`inv schema <op>`), mirroring that JIT pattern in MCP cuts startup token tax by ~85–90% while keeping identical validation and repairable error envelopes.

**Benefits:**
- Reduces default MCP prompt overhead from ~15k–30k+ tokens down to <3k tokens.
- Eliminates tool-choice ambiguity across 10 overlapping write-group tools and avoids LLM `oneOf` schema incompatibilities.
- Preserves full single-call cross-entity atomic transactions (`retrospective.record` + `case.close`, or `observation.add` + `assessment.add`) natively in the default profile without needing an "advanced" `record_batch` escape hatch.

**Trade-offs:**
- The agent typically calls `inv_schema` once before writing an unfamiliar operation type (identical to CLI UX, and amortized across the session).
- Host-side static UI auto-forms are less granular in the default profile (addressed by `--profile expanded-typed` when desired).

**Implementation Notes:**
- Add an automated CI check measuring serialized byte/token size of `tools/list` output for the default MCP profile and failing if it exceeds the budget ceiling (e.g., 12 KiB / ~3,000 tokens).
- Keep exact validation, error paths, and minimal-example repair guidance identical between `inv record` and `inv_record`.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1053,11 +1053,14 @@
 
 The CLI is the reference implementation. MCP uses the same Rust command dispatch, validation, receipts, and error codes over stdio. No HTTP listener is needed.
 
-Expose read tools `onboard`, `brief`, `next`, `frontier`, `blindspots`, `retrospective`, `get` (with `depth`), `search`, `check`, and `schema`. Expose domain writes through grouped, typed tools such as `capture`, `observations`, `hypotheses`, `inquiries`, `comparisons`, `assessments`, `conclusions`, and `decisions`, plus `reviews` (including challenge reviews) and `retrospectives`. Each write group uses a closed operation union generated from the catalog, with a shared transaction envelope. `capture` may atomically create linked source/artifact/observation records. Where a cross-group transaction is needed, provide a fully typed `record_batch` tool in an explicitly selected advanced profile.
+To prevent eager MCP schema bloat (which degrades LLM tool selection and consumes 15k–55k prompt tokens) and avoid provider incompatibilities with deeply nested `oneOf` unions, ship a **compact JIT-discovery default profile** (`--profile core`, default) capped at **≤7 flat-schema tools and <12 KiB (~3,000 tokens) total `tools/list` manifest size**:
 
-Do not hide unvalidated JSON strings behind a supposedly typed tool. Do not duplicate all CLI aliases as tools. Measure schema/token overhead with actual clients; keep a core profile and a fuller workflow profile if useful, advertised at startup. Clients without discovery/load-on-demand still receive a sufficient, stable core surface.
+- `inv_onboard`: workspace/case discovery and next valid step.
+- `inv_read`: flat-parameter read dispatch (`action: brief | show | explain | search | next | frontier | blindspots | retrospective | check`, with `depth`, `case_id`, `id`, `query`, `max_bytes`).
+- `inv_evidence_read`: explicit opt-in bounded artifact excerpt reader with untrusted-content marking.
+- `inv_schema`: JIT catalog lookup returning exact JSON Schema, purpose, and minimal valid example for any operation.
+- `inv_record`: validated atomic transaction executor (same envelope/validation/repair semantics as `inv record`, supporting cross-entity batches natively without a separate escape hatch).
+- `inv_ingest`: local source/artifact ingestion.
+
+Provide an opt-in `--profile expanded-typed` mode exposing domain-grouped typed tools for MCP hosts that implement lazy/on-demand tool search. Never accept opaque unvalidated strings as silent mutations; all payloads validate against the canonical operation catalog with field-level repair errors. Measure `tools/list` token overhead and tool-call accuracy across both profiles in §20.5.
 
 Resources expose versioned schemas, method cards, and sanitized case snapshots. Read-only tool annotations and mutation annotations must match real effects. Never advertise hypothetical tools from an unavailable capability. MCP is a transport, not a second source of truth.
```

---

### [Medium Impact, Low Effort] Change #10: Strengthen Prompt-Injection Defense Against Malicious Valid `inv record` Writes (CaMeL-Inspired Taint & Promotion Guardrails) (§16.1, §20.5)

**Current State:**  
§16.1 focuses on preventing hostile source content from modifying schemas, executing shell commands, or installing plugins, and adds Markdown fence sanitization + untrusted labels. §20.5 Scenario #7 tests only a forged `AGENTS.md` or instruction text inside a source.

**Proposed Change:**  
Address the realistic **in-band epistemic corruption attack** where indirect prompt injection inside a captured log/webpage hijacks the LLM into issuing *completely schema-valid* `inv record` mutations (e.g., validating a false conclusion, withdrawing contradicting evidence, or downgrading sensitivity classification):
1. **Add optional capability/taint guardrails at the CLI/MCP boundary (§16.1):**
   - Support a `--read-only` mode (for untrusted raw-evidence triage sessions / subagents) that physically disables mutation and export commands.
   - In high-rigor/adversarial cases (opt-in policy or default for `restricted` sensitivity downgrades), support **promotion-gate separation**: mutations that *promote* (`conclusion.validate`, `decision.accept`), *suppress* (`source.withdraw`, `observation.retract`), or *downgrade classification* can require either a distinct review actor/fresh-context confirmation token or explicit human approval (`--confirm-promotion-token` / companion approval), preventing a single raw-evidence-tainted agent session from silently corrupting canonical verdicts.
2. **Upgrade Adversarial Drill #7 in §20.5:** Test an injected evidence artifact that contains a *valid, copy-pasteable `inv record` JSON payload* instructing the agent to reject the true hypothesis or validate a false conclusion—verifying both agent resistance and structural guardrail enforcement.

**Rationale:**  
*Debenedetti et al. (2025), "CaMeL: Defeating Prompt Injections by Design"* demonstrates that prompt-level untrusted-data framing alone does not reliably stop indirect prompt injection in tool-using agents; security requires **structural control-flow and capability boundaries** between untrusted data ingestion and privileged state transitions. Because `inv` is an epistemic ledger, the highest-value target for an attacker embedding prompt injection inside a scraped web page, crash log, or repository file isn't `rm -rf`—it is **poisoning the investigation's own conclusion, suppressing counter-evidence, or exfiltrating `restricted` data via sensitivity downgrade**.

**Benefits:**
- Closes the most realistic prompt-injection threat model specific to an investigation workbench.
- Allows orchestrators to run a quarantined `--read-only` reader agent on hostile raw captures while keeping promotion writes in a clean context (mirroring CaMeL's dual-capability architecture without complicating `inv` core).

**Trade-offs:**
- High-rigor promotion separation is opt-in via workspace/pack policy so low-friction solo workflows aren't burdened by confirmation tokens.

**Implementation Notes:**
- `--read-only` flag on `inv` / `inv mcp --read-only` is trivial to implement at the command-dispatch layer (rejects all mutating commands before acquiring the commit lock and omits write tools from MCP `tools/list`).
- Audit logs should note when `evidence read` occurred in the same actor session immediately preceding a promotion or withdrawal.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1599,6 +1599,11 @@
 Structured output labels evidence excerpts as untrusted. Quoting with a Markdown `>` alone does not make content inert. Render excerpts in escaped plain text or safely delimited code blocks with a fixed non-executable language; choose a delimiter longer than any matching run in the excerpt. Never use a source-provided fence language. Sanitize HTML and generated labels, and construct internal links from validated object IDs. Disable remote embeds and command-capable URI links in captured content.
 
-These are product integrity controls for agents consuming adversarial evidence, not a claim that text labeling alone prevents every prompt-injection failure.
+Because text labeling alone cannot defeat indirect prompt injection—especially attacks that trick an agent into issuing *schema-valid* `inv record` commands to poison conclusions, retract counter-evidence, or downgrade `restricted` sensitivity—provide structural capability boundaries inspired by capability-based isolation (CaMeL):
+
+- **`--read-only` mode** (`inv --read-only`, `inv mcp --read-only`): disables all mutating/export endpoints at the dispatch layer so hosts can quarantine raw untrusted-artifact inspection in a read-only agent context.
+- **Optional promotion/suppression separation (policy-configurable):** high-rigor packs or `restricted` workspaces may require distinct actor/session isolation or explicit human/companion confirmation for consequential transitions (`conclusion.validate`, `decision.accept`, `source.withdraw`, and classification downgrades), preventing a single evidence-tainted context from unilaterally altering canonical verdicts or export visibility.
+
+These structural boundaries reduce prompt-injection blast radius without claiming to solve model-level prompt injection universally.
@@ -1999,7 +2004,7 @@
 4. Compare architectural options under hard and soft constraints, record a conditional decision and react to a changed criterion.
 5. Resume interrupted work with a different agent and a stale snapshot.
 6. Retry a write whose response was lost; handle a real revision conflict without clobbering.
-7. Receive a source containing a forged `AGENTS.md` or instructions and ignore its requests.
+7. Receive a source containing both a forged `AGENTS.md` and an embedded schema-valid `inv record` JSON payload urging the agent to reject the true cause or validate a false conclusion; verify the agent ignores the injected mutation and that `--read-only` / policy guardrails physically block unauthorized promotions.
 8. Use title/summary reads to inspect a 40-record case before selectively fetching deep evidence; retain the decisive caveat and reduce unnecessary raw reads.
```

---

### [Medium Impact, Low Effort] Change #11: Fix Obsidian Bases `.base` Syntax (`note.` Prefixes, `properties:` Display Names, `sort:` vs. `order:`) (§12.5)

**Current State:**  
The `.base` YAML example in §12.5 (lines 1363–1376) omits the `note.` prefix on custom frontmatter properties inside `order:` and omits `properties:` customization and `sort:` blocks, while filtering broadly on `file.inFolder("Cases")` rather than an exact case scope.

**Proposed Change:**  
Update the §12.5 scaffold to match verified official Obsidian Bases YAML syntax:
1. Scope `filters` to the exact case folder (`file.inFolder("Cases/<case-id>")`) AND `inv_case_id == "<case-id>"` AND `inv_kind == "conclusion"`.
2. Add a `properties:` block using mandatory `note.<prop>` prefixes (e.g., `note.inv_effective`, `note.inv_status`, `note.inv_revision`) with clean human-readable `displayName` labels so users aren't staring at raw `inv_*` identifiers.
3. Use `note.` prefixes consistently in `order:` (which controls column sequence) and add an explicit `sort:` block (which controls row ordering).

**Rationale:**  
Per official Obsidian Bases documentation (`obsidian.md/help/bases/syntax`), omitting the `note.` prefix in the `properties:` configuration block causes Obsidian to silently ignore display customizations, and `order:` specifies **column layout**, not row sort order (which requires `sort:`). Additionally, rendering readable `displayName` headers dramatically improves §20.6 human-experience usability without giving up the safe flat `inv_*` frontmatter namespace.

**Benefits:**
- Prevents silent broken/ugly `.base` files when implementing W12.
- Makes generated Obsidian dashboards clean and human-friendly (`Effective State`, `Status`, `Rev`) while preserving vault-wide collision-free `inv_*` property names.

**Trade-offs:**
- Minor extra verbosity in generated `.base` YAML templates.

**Implementation Notes:**
- Still validate generated `.base` files against the pinned Obsidian desktop release during W12 integration tests (§12.3), as Bases syntax can evolve across minor Obsidian releases.

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1361,16 +1361,30 @@
 Generate native `.base` files with filters on flat properties, and plain Markdown fallback tables. Verify the syntax against the actual supported Obsidian release [O1,O2]. Example scaffold (validate/revise in implementation):
 
 ```yaml
 filters:
   and:
-    - 'file.inFolder("Cases")'
+    - 'file.inFolder("Cases/<case-id>")'
+    - 'inv_case_id == "<case-id>"'
     - 'inv_kind == "conclusion"'
+properties:
+  note.inv_effective:
+    displayName: "Effective State"
+  note.inv_status:
+    displayName: "Status"
+  note.inv_revision:
+    displayName: "Rev"
+  note.inv_updated:
+    displayName: "Updated"
 views:
   - type: table
     name: Conclusions
+    sort:
+      - property: note.inv_updated
+        direction: DESC
     order:
       - file.name
-      - inv_effective
-      - inv_status
-      - inv_revision
+      - note.inv_effective
+      - note.inv_status
+      - note.inv_revision
+      - note.inv_updated
 ```
```

---

### [Medium Impact, Low Effort] Change #12: Add a Milestone A1 Minimum-Viable-Core Cold-Agent Validation Gate Before Freezing the Full 18-Type Schema (§19.1–§19.3)

**Current State:**  
§19.1 warns *"Do not begin with a huge ontology, plugin, task runner or dynamic-method language,"* yet W01 immediately freezes **18+ domain record types** (§5.2) and ~50 operations (§9.4), and cold-agent testing doesn't occur until **W14 (Milestone C)**—after storage, ingestion, producer APIs, Obsidian rendering, method packs, exports, and MCP are all built!

**Proposed Change:**  
Insert an explicit **Milestone A1 Cold-Agent Validation Checkpoint** directly after W05 (before W07–W13):
1. **Start W01 with the Minimum Viable Core (~10 record types):** `Case`, `Question`, `Source`, `Artifact`, `Observation`, `Hypothesis`, `Option`, `Criterion`, `Comparison`, `Assessment`, `Conclusion`, `Decision`, `Review`, `InquiryPlan`, `Run` (plus lightweight `Assumption`). Defer `CoveragePlan/Cell`, authored `annotation(role=synopsis)`, custom method-pack schema extensions, and `Retrospective` schema freeze until after Milestone A1 trials.
2. **Run an early paired cold-agent trial (2 domains: Debugging + Architecture Choice, treatment vs. plain-notes control) at the end of Milestone A**—not waiting until W14! Use empirical failure traces from real unfamiliar agents to prune confusing operations, simplify envelopes, and verify that schema complexity isn't causing agents to stall or drop accuracy relative to plain Markdown.

**Rationale:**  
Waiting until **W14** (nearly the end of the entire roadmap) to discover that cold agents struggle with 18 record types, 50 operations, or CAS revision workflows is classic waterfall risk. As Dhami et al. (2019) observed with human analysts abandoning complex ACH steps, **cognitive overhead kills methodological compliance**. Running a cheap 2-domain paired cold-agent trial at Milestone A validates the core ontology *before* locking in cross-language producer SDKs (W07), Obsidian renderers (W08/W12), exports (W11), and MCP adapters (W13) against a potentially awkward schema.

**Benefits:**
- De-risks the entire W07–W14 build by testing real agent ergonomics early when schema refactoring is cheap.
- Resolves the internal contradiction between §19.1 ("don't start with a huge ontology") and W01 (freezing 18+ types up front).
- Ensures deferred features (`CoveragePlan/Cell`, authored synopses, companion plugin) earn their complexity empirically.

**Trade-offs:**
- Minor schema evolution (`v0` → `v1`) may be needed between Milestone A1 and Milestone B if cold-agent trials uncover ergonomic issues—which is far cheaper than refactoring after W13.

**Implementation Notes:**
- Mark Milestone A schemas explicitly as `inv.command.v0-alpha` until the Milestone A1 cold-agent checkpoint passes; freeze `inv.command.v1` at the start of Milestone B (W06/W07).

**Git-Diff:**
```diff
--- notes/plan.md
+++ notes/plan.md
@@ -1817,7 +1817,7 @@
 | ID | Package | Depends on | Demonstrated acceptance |
 |---|---|---|---|
 | W00 | Read destination repo instructions; create ADRs, fixtures, support matrix, baseline toolchain and CI. | — | Domain boundaries/protocol drafts committed and tests runnable. |
-| W01 | Domain types: Case/Question/Source/Artifact/Observation/Hypothesis/Option/Criterion/Comparison/Assessment/Conclusion/Decision/Review/**Retrospective**/Inquiry/Run. Single operation catalog; review `kind: blindspot` and `annotation role: synopsis`. | W00 | Schema-valid minimal operations and independently compiling core. |
+| W01 | Minimum-viable core types (`v0-alpha`): Case/Question/Source/Artifact/Observation/Hypothesis/Option/Criterion/Comparison/Assumption/Assessment/Conclusion/Decision/Review/Inquiry/Run. Single operation catalog. Defer `CoveragePlan/Cell`, authored `synopsis` annotations, and `Retrospective` freeze until Milestone A1 validates agent ergonomics. | W00 | Schema-valid minimal operations and independently compiling core. |
 | W02 | Durable event journal and blob store, CAS, canonical hashing, idempotent receipts, crash recovery. | W01 | Deterministic replay + fault-injection suite passes. |
 | W03 | Minimal explanatory loop, initial observations and artifact provenance, tests/runs, assessments, conclusions and review. | W01, W02 | Debug scenario works without special consumer and retains original evidence. |
 | W04 | Minimal evaluative loop: options, criteria, comparisons, decisions, sensitivity questions and supersession. | W01, W02 | Choice scenario uses same core, no forced hypotheses. |
@@ -1836,9 +1836,11 @@
 
 ### 19.3 Milestones
 
-**A — undeniable vertical slice:** W00–W04 plus minimum W05/W06. Two cases work: debugging and architecture choice, using the same store and CLI. A reviewer sees exact support/assumptions, layered read depths, and a second agent can resume. Early cases may close only with a minimal truthful Retrospective.
+**A — undeniable vertical slice (`v0-alpha`):** W00–W04 plus minimum W05/W06. Two cases work: debugging and architecture choice, using the same store and CLI. A reviewer sees exact support/assumptions, deterministic layered read depths, and a second agent can resume.
+
+**A1 — early cold-agent ergonomics gate (before `v1` freeze):** Run a pilot paired cold-agent evaluation (treatment vs. plain-notes control on Debugging + Architecture Choice) *before* starting W07–W13. Refactor any operation, envelope, or schema friction uncovered by real agents, then freeze `inv.command.v1` and add `Retrospective`, authored synopses, and optional `CoveragePlan/Cell` only where empirically justified.
 
 **B — generic recording and honest knowledge:** W06–W10. External producer fixtures (not a named app) record runs; provenance, validity, independence, criteria changes, synopsis freshness, and structural blindspot review are correct. No false security-/performance-specific defaults leak into the core.
```

---

## Research References Used to Challenge Assumptions

| Topic | Source | Key Empirical / Technical Finding Applied |
|---|---|---|
| ACH empirical efficacy | Mandel et al. (2018), *Boosting intelligence analysts' judgment accuracy*; Dhami et al. (2019), *Applied Cognitive Psychology* | Qualitative ACH shows mixed/null debiasing effects and can increase inconsistency; flexible evidence structure + statistical/diagnostic rigor outperforms rigid matrix rituals. |
| Severe Testing | Deborah Mayo (2018), *Statistical Inference as Severe Testing* | Evidence supports $H$ only to the extent the test had high probability of detecting flaws were $H$ false (Change #5). |
| Strong Inference & Causal DAGs | John Platt (1964), *Strong Inference*; Judea Pearl, *Causality* | Multiple rival hypotheses + crucial discriminating tests; strict distinction between confounders (control), mediators (preserve), and colliders (never filter/condition on joint effects) (Change #5). |
| Analytic Uncertainty & Source Grading | ODNI ICD 203; GRADE Working Group; NATO STANAG 2511 (Admiralty Code); Irwin & Mandel (2019) | Never conflate likelihood with analytic confidence; anchor evidence certainty in 5 explicit bias/consistency/directness/precision/reporting axes; decouple source reliability from information credibility (Change #2). |
| Decision Science | Gary Klein (2007), *Performing a Project Premortem*; Bent Flyvbjerg, *Reference Class Forecasting*; Pareto Dominance & Value of Information (Howard/Raiffa) | Weight-free Pareto frontier pruning, VoI-prioritized measurement, outside-view base rates, and prospective-hindsight premortems (Change #6). |
| Reporting & Selection Bias | PRISMA 2020 Guidelines; Cochrane Risk of Bias | Systematic search provenance and negative-search logging to distinguish verified absence from unsearched corpora (Change #7). |
| LLM Self-Correction & Bias | Huang et al. (ICLR 2024), *Large Language Models Cannot Self-Correct Reasoning Yet*; 2025 LLM sycophancy & anchoring literature | Intrinsic same-session LLM self-critique without external feedback fails or degrades reasoning; mandatory documentation gates invite boilerplate compliance (Change #3). |
| LLM Prompt Injection | Debenedetti et al. (2025), *CaMeL: Defeating Prompt Injections by Design* | Text delimiters do not stop indirect prompt injection into valid tool writes; structural capability/read-only isolation is required (Change #10). |
| Eval Statistics | Evan Miller (Anthropic, Nov 2024), *Adding Error Bars to Evals* | Paired baseline controls, confidence intervals, clustered standard errors, and power analysis are required to make valid claims from LLM evals (Change #1). |
| MCP Tool Overhead | Anthropic / Blocks / Atlassian (`mcp-compressor`) 2025–2026 MCP analyses | Eager loading of 20+ nested union-schema tools adds 15k–55k prompt tokens and degrades tool selection; compact JIT-discovery schemas cut overhead ~85–90% (Change #9). |
| Linux Storage & JCS | Linux `renameat2(2)` / `flock(2)` / `fcntl(2)` / ext4 `jbd2` WAL literature; RFC 8785 (JCS) cross-language audits | Segmented append-only WAL outperforms one-file-per-event + dir `fsync`; `fcntl` drops locks on any FD close; `rename` clobbers; JCS floats diverge between Rust and Python on `-0.0`/`1e+21`/`1.0` (Changes #4, #8). |
| Obsidian Bases | Official Obsidian Help (`obsidian.md/help/bases/syntax`) | `properties:` customization requires `note.<prop>` prefix; `order:` governs columns while `sort:` governs row ordering (Change #11). |
