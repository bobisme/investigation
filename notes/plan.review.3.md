# Review 3 of the Investigation implementation plan

## Executive summary

**Before freezing the schema, test whether a fresh investigator reaches a better-supported answer with Investigation than with a Markdown notebook.** Give one investigator the evidence, interrupt them, and ask their successor what can be concluded, what could overturn it, and where to check. A receipt can establish that the work was saved. The successor's answer tests whether it was worth saving in this form.

Revision 3 gets several expensive decisions right. It preserves original bytes, keeps producer assertions separate from investigator judgments, and lets a person choose an option under uncertainty. The offline core, disposable index and human-owned notes make the case usable after the original chat disappears. The gaps show up where those parts meet. A review can go stale when a *new* criterion appears. A newly collected article can repeat a previously known result. A public report can lose its only material objection during filtering. Each can change the answer a reader takes away even when every stored byte is correct.

| When | Decision to make | Changes |
|---|---|---|
| Before freezing the public model | Define a successful handoff, what belongs to a review, what counts as independent evidence and what the investigator knew before testing. | 1–4 |
| In the first runnable slice | Make starting with a real source, recovering a lost reply and choosing the next question work from the ordinary CLI. | 5–6 |
| With the performance and recorder fixtures | Decide how missing measurements constrain a claim and when a producer can call a recording complete. | 7–8 |
| Before sharing or expiring evidence | Keep caveats intact across audience boundaries and preserve shared bytes for every entitled case. | 9–10 |

**Decision on the earlier reviews.** Carry forward the plain-notes baseline, held-out cases, mechanically checked quotations, and a retrospective grounded in the journal. A quote match establishes that the words occurred at a selector. Reviewing whether they justify the claim still belongs in the workflow. Keep the existing local journal shape for the first measurement. The two earlier reviews propose incompatible storage layouts, and this workspace contains a specification and reviews without workload measurements that decide between them. The proposals below target revision 3 directly and can be evaluated without applying either earlier review. [Plan](plan.md) · [Review 1](plan.review.1.md) · [Review 2](plan.review.2.md)

## Proposed changes

The effort estimates cover implementation and its named test. Each expandable block is a complete unified diff. The ten blocks also apply in the order shown. Section and line references in the findings refer to the original revision.

### 1. Measure better investigations against the notebook people already use

**Priority** High impact, medium effort. Set the rules in W00 and test the first two workflows at Milestone A.

Section 20.5 tests task success and false promotion across a small set of named scenarios. The aggregate can hide unwarranted abstentions on cases with enough evidence. Every trial gives the agent an investigation to perform, so the gate also misses the entry decision in §1.1. The 50% reduction against reading *every* record measures compression against an expensive baseline. A notebook user can skim too.

In the human experiment already cited in §2, analysts trained in Analysis of Competing Hypotheses evaluated cue usefulness more accurately, yet their probability judgments showed no significant accuracy gain. This is a reason to score the final judgment separately from how well an investigator fills in the evidence matrix. Agent handoffs require their own trial. [Mandel et al., judgment accuracy, §§3.2–3.3](https://doi.org/10.1017/S1930297500006628)

**Evidence and rationale** Behavioral tests can distinguish a change that should leave an answer invariant from one that should change it. Research on selective prediction also shows why reporting errors only among answers, or only counting wrong promotions, leaves out how often the system answers at all. These studies inform the trial design. The effect on this workbench remains an empirical question. [Ribeiro et al., CheckList, §2](https://aclanthology.org/2020.acl-main.442.pdf) · [Geifman and El-Yaniv, selective classification](https://proceedings.neurips.cc/paper/2017/hash/4a8423d5e91fda00bb7e46540e2b0cf1-Abstract.html)

**Proposal and benefit** Pair each held-out case with a Markdown condition that has the same files, ordinary search, budget and handoff opportunity. Make the primary outcome a correct, appropriately bounded answer by the *successor*, including the decisive citation and strongest limitation. A neutral rubric can credit different defensible architecture choices. Include a sufficient-evidence twin, a missing-evidence twin, and a cosmetically changed twin. Count missed warranted answers alongside unwarranted promotions. Include a one-shot lookup where opening a case adds no value.

**Cost and interpretation** Independent cases and blinded grading cost more than replaying ten scripts. Use cheap deterministic integrity checks on every change, exploratory trials at A, and reserve separate sealed cases for a release claim. Even under independent, equal-risk trials, zero failures in 20 gives a one-sided 95% exact upper bound of about 13.9% on the failure probability, from `1 − 0.05^(1/20)`. Repeating one case 20 times supplies much less evidence about *other cases*. If neither accuracy nor effort improves by the declared margin, simplify the workflow before expanding it.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -1818,5 +1818,5 @@
 |---|---|---|---|
 | W00 | Read destination repo instructions; create ADRs, fixtures, support matrix, baseline toolchain and CI. | — | Domain boundaries/protocol drafts committed and tests runnable. |
-| W01 | Domain types: Case/Question/Source/Artifact/Observation/Hypothesis/Option/Criterion/Comparison/Assessment/Conclusion/Decision/Review/**Retrospective**/Inquiry/Run. Single operation catalog; review `kind: blindspot` and `annotation role: synopsis`. | W00 | Schema-valid minimal operations and independently compiling core. |
+| W01 | Draft the common envelope, identity, provenance and the domain types exercised in W03/W04. Reserve names for later operations. | W00 | An independently compiling core, schema-valid examples and a minimal handoff fixture. Freeze the wider catalog after the Milestone A comparison. |
 | W02 | Durable event journal and blob store, CAS, canonical hashing, idempotent receipts, crash recovery. | W01 | Deterministic replay + fault-injection suite passes. |
 | W03 | Minimal explanatory loop, initial observations and artifact provenance, tests/runs, assessments, conclusions and review. | W01, W02 | Debug scenario works without special consumer and retains original evidence. |
@@ -1839,4 +1839,6 @@
 **A — undeniable vertical slice:** W00–W04 plus minimum W05/W06. Two cases work: debugging and architecture choice, using the same store and CLI. A reviewer sees exact support/assumptions, layered read depths, and a second agent can resume. Early cases may close only with a minimal truthful Retrospective.
 
+At A, run a two-domain paired pilot against ordinary Markdown notes using §20.5. Keep lessons and failure traces as development data. Before expanding the catalog, use the pilot to remove unnecessary steps, estimate between-case variability and set a meaningful benefit and burden limit for the independent release trial.
+
 **B — generic recording and honest knowledge:** W06–W10. External producer fixtures (not a named app) record runs; provenance, validity, independence, criteria changes, synopsis freshness, and structural blindspot review are correct. No false security-/performance-specific defaults leak into the core.
 
@@ -1849,5 +1851,5 @@
 ### 19.4 Parallel agent lanes
 
-After W01 freezes core schema and receipt semantics:
+After W01 fixes the envelope, identity and receipt semantics for the first slice, develop the domain catalog against shared fixtures. Freeze its wider public surface after Milestone A.
 
 - **Storage lane:** journal, blobs, hashing, recovery, import.
@@ -1990,5 +1992,7 @@
 ### 20.5 Cold-agent evaluation
 
-This is a release gate, not a documentation review. At minimum test two distinct agent families/hosts available to implementers, multiple fresh sessions, recorded prompt+tool version, and an independent scenario oracle. Each agent receives **only** the executable, a seed data bundle, a task objective and permission to use `--help`. No hidden domain-specific coaching from the evaluator.
+Use at least two agent families or hosts. The experimental unit for generalizing across investigations is an independently authored case. Match each case across two conditions, plain Markdown notes with normal file/search tools, and those same tools plus `inv`. Give separate fresh sessions the same objective, source bytes, time/token budget and handoff boundary. Offer discovery through ordinary help. Give neither condition evaluator coaching. Counterbalance run order, pin model/host/tool versions and prevent session carryover. Split development, pilot and sealed evaluation cases by underlying case family, including all variants in the same split. Keep the evaluation keys outside the workspace seen by the agents.
+
+Both conditions submit the same short answer format. Normalize citations to seed-document selectors and strip tool identifiers before independent graders see the answers. Preserve the original outputs for audit. Score the successor's bounded answer, precise evidence, material caveat and decision or justified abstention against a prewritten rubric. Allow multiple choices consistent with the stated preferences and hard constraints. Record grading disagreements and resolve them against sources. Grade tool-specific mechanics separately. Use paired changes that remove a decisive source, flip an operative criterion, rename an option or add a duplicate citation. Include tasks where the appropriate action is to skip the tool.
 
 Four base scenarios plus adversarial variants:
@@ -2010,16 +2014,16 @@
 |---|---|
 | Correct case/object targeting | 100%; no cross-case writes. |
-| Simple first useful commit | Within three tool calls after onboarding/schema access. |
-| Task success | At least 90% over at least 20 documented fresh runs, with each domain represented and the new blindspot/retrospective scenarios exercised; investigate every failure. |
-| False promotion | Zero in invalid-control, source-dependence and stale-decision cases. |
+| First use and total burden | Retain the target of at most three calls after onboarding/schema access for the first useful receipt. Also count all calls, help/schema bytes, file preparation, elapsed time and tokens from a fresh start through handoff in each condition. |
+| Task success | Treat 18 successes in 20 as a smoke target. For a release benefit claim, preregister the primary handoff outcome, minimum useful difference, acceptable burden and sample size from pilot variability across cases. Report paired differences and uncertainty grouped by case, with model/domain breakdowns. |
+| False promotion and unnecessary abstention | Require zero false promotions in deterministic invalid-control and stale-basis fixtures. In agent trials give counts and denominators for unwarranted answers on insufficient evidence and warranted answers reached on sufficient evidence. Count all assigned tasks, including timeouts. |
 | Lost/duplicate writes | Zero with injected response loss, retries and process termination. |
 | Repairability | Ordinary schema/CAS error resolved without bespoke intervention. |
 | Continuation | Fresh agent accurately states the current answer/choice, its strongest caveat, and next useful observation. |
-| Context efficiency | Measure bytes/tokens and unnecessary calls; title/summary reads reduce output versus full evidence while preserving necessary caveats. A compact 40-record fixture should emit at least 50% fewer bytes than a full-record traversal without worse answer correctness. This is a release *target*, not a claimed result. |
+| Context efficiency | Keep the 50% compact-versus-full byte reduction on the 40-record fixture as a compression regression check. For the product comparison, measure the actual selective reads, end-to-end tokens, time and correctness in both conditions. |
 | Blindspot calibration | All seeded structural gaps detected in matched fixtures; no claim of completeness for unknown semantic gaps. Reviewer can record additional gaps safely. |
 | Learning discipline | Every completed substantive scenario has a linked, useful Retrospective; zero invented failures or auto-adopted method changes. |
 | Domain neutrality | Agents in architecture/research tasks do not need security abstractions or unrelated test-run ceremony. |
 
-These are release targets, not measured results. Don't cheat by embedding fixture-specific solutions in onboarding or source labels. Compare against independent expected evidence/decision properties, not just tool-call completion.
+Before opening the sealed set, choose one release criterion: a meaningful gain in handoff correctness within the cost budget, or noninferior correctness within a stated margin with a meaningful reduction in burden. Use the prespecified interval and decision rule. If results are inconclusive, report the uncertainty and withhold the improvement claim. Use fresh held-out families for the next release comparison after tuning on failures.
 
 ### 20.6 Human experience gate
@@ -2258,5 +2262,5 @@
 ## Final handoff instruction
 
-Start with W00/W01 to freeze generic data and transaction contracts. Deliver Milestone A as a working **explanatory** case and a working **architecture-choice** case. Then make the neutral recorder reusable by independent consumers, qualify crash recovery and provenance, and ship a headless Obsidian casebook. The fresh-agent tests in §20 are as important as durability tests: one measures whether unfamiliar agents actually know when/how to use the tool; the other whether recorded knowledge survives real errors.
+Start with W00/W01 to fix the transaction envelope and draft the domain slice for the Milestone A comparison. Deliver Milestone A as a working **explanatory** case and a working **architecture-choice** case. Then make the neutral recorder reusable by independent consumers, qualify crash recovery and provenance, and ship a headless Obsidian casebook. The fresh-agent tests in §20 are as important as durability tests: one measures whether unfamiliar agents actually know when/how to use the tool; the other whether recorded knowledge survives real errors.
 
 **The result should feel simple:** *ask a question; capture evidence; compare explanations or choices; gather discriminating information; challenge what you may have missed; reach a bounded answer or decision; and leave an understandable case **and a useful method retrospective** for the next investigator.*
````

</details>

### 2. Make a new criterion or objection capable of invalidating an old review

**Priority** High impact, medium effort. Decide in W01 and exercise it with the first two decisions.

At snapshot 40 a reviewer sees every hard criterion for the queue decision and no critical objections. At 41 someone adds a new applicable recovery requirement. Every ID the reviewer read at 40 can still have the same revision. Sections 5.7 and 10.3 require the core to compute the dependency closure, but leave open how it remembers that *all applicable criteria* or *no open objections* were part of the judgment. Checking a larger closure at the moment of acceptance alone also misses a fresh insertion **after** acceptance.

**Evidence and rationale** Database isolation literature calls a changed set of rows matching the same condition a phantom. SQLite can give one consistent view to a read transaction. A human review can span many commands and outlive that transaction. Its basis therefore needs a durable description of which set was reviewed. This is an application-level design inference and works with the existing writer lock. [PostgreSQL transaction isolation definitions](https://www.postgresql.org/docs/18/transaction-iso.html) · [SQLite isolation](https://www.sqlite.org/isolation.html)

**Proposal and benefit** Scope options and criteria to the decision question and objections to their subject. Store a versioned rule and fingerprint for each material set, including the empty set, alongside the exact revisions already required. Check it on promotion and on later reads. A user following an old handoff should be able to inspect that snapshot and see what changed since. A second, unrelated question in the same case should keep its current standing.

**Implementation and cost** Start with indexed membership queries and a deterministic digest over sorted IDs and substantive bases. Compare incremental results with a full recomputation on fixtures, including insert, retire, relink and replay. Support a small fixed set of predicates in the core. Arbitrary saved SQL and a workspace-wide invalidation counter would both add cost. This solution detects changes expressible in recorded links. A new fact whose relevance nobody has identified still needs a person or agent to connect it.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -381,4 +381,5 @@
 | **Conclusion** | Bounded synthesized claim about what was learned | Statement, significance, scope, support/contradictions, review and effective disposition. |
 | **Review** | Attributed review of a case or a conclusion, comparison or decision | Review kind (`validation` or `blindspot`), exact subject/read-set, method, materials examined, source independence, objections/gaps, verdict and caveats. |
+| **Objection** | Attributed material challenge to a claim or decision | Subject/revision, stated scope, severity, rationale, supporting evidence or explicit unknown, originating Review if applicable, disposition and revision history. |
 | **Retrospective** | Attributed methodological account of an investigation | Case/snapshot, methods attempted, productive/invalid approaches, costs or constraints, linked examples, lessons and transfer limits; supersedable, not auto-applied. |
 | **Decision** | A chosen option or resolved question, with reasons | Choice, compared alternatives, criteria, evidence, tradeoffs, consequences, reopen trigger. |
@@ -542,5 +543,5 @@
 | Recommend/accept a decision | Candidate options and applicable active criteria, hard constraint disposition, material unknowns, rationale, consequences, reversibility/reopen trigger. | Does the trade justify the choice? |
 | Promote conclusion to review-ready | Bounded claim, direct support, applicability, alternative explanation treatment, limitations and inspectable evidence. | Is it convincing? |
-| Validate conclusion | Valid review/read-set, no unresolved critical blockers; method-specific policy obligations met. | Is residual uncertainty acceptable? |
+| Validate conclusion | Current submitted validation review with a supportive verdict on the exact claim basis, current material sets, satisfied method obligations and no unresolved critical blocker. | Is the scoped conclusion justified despite remaining caveats? |
 | Blindspot review (optional by default) | `inv blindspots` checks recorded structure; `review.submit` with `kind: blindspot` captures reviewed scope, gaps and exact read-set. | Did the investigator actively challenge omissions and rival accounts? A clean checklist is not proof of completeness. |
 | Close substantive case | Answer/choice/outcome or explicit inconclusive reason; outstanding questions, limitations, reopen triggers; attributed method Retrospective in same or prior transaction. `record`-only sessions are exempt. | Was this investigation sufficient, and what did its process teach? |
@@ -715,4 +716,6 @@
 
 `inv brief --since <transaction-id>` returns a semantic delta, not the entire case. Include critical gaps, retrospective/lesson references and any stale synopsis in the delta when relevant. `inv show ID --at <revision>` gives historical state. `inv explain ID` walks the recorded evidence/assessment chain with bounded depth; it is a deterministic presentation of committed rationale, not newly generated reasoning.
+
+An evidence-navigation link in a brief carries the workspace and exact snapshot. Support `--snapshot TRANSACTION_ID` for linked `show`, `explain` and `brief` reads. Resolve the full graph as known at that head and label it historical. The default remains the current head. Evaluate what can be disclosed or physically read under today's audience and retention policy, including for historical links.
 
 ### 8.4 Search and evidence reading
@@ -901,5 +904,5 @@
 | `conclusion.validate` / `conclusion.disposition` | Validate review-ready scoped claim or reject/retract/supersede, with blockers/read-set. |
 | `decision.propose` / `decision.accept` / `decision.supersede` | Exact selected option or choice, alternatives/criteria, hard constraints, evidence, assumptions, tradeoffs, consequences and reopening triggers; accepted is immutable. |
-| `objection.resolve` | Identified blocker/review concern revisions, rationale/evidence, update freshness via dependency graph not arbitrary flags. |
+| `objection.raise` / `objection.resolve` | Raise a subject-scoped challenge with a stable ID, including alongside a Review in one batch. Resolve that exact objection revision with an attributed reason and evidence. Both operations update the applicable set of open challenges. |
 | `coverage.define` / `coverage.revise` / `coverage.link` | Optional bounded applicability matrix, per-cell predicate and linked measurements, denominator history. |
 
@@ -1103,4 +1106,8 @@
 The core computes that required dependency closure. The caller supplies expected revisions, but cannot omit an inconvenient dependency to bypass a gate. A missing or changed required read-set member produces a repairable rejection with the current required set. Detect circular justification while evaluating sufficiency.
 
+A review basis includes the membership of material sets as well as individual revisions. Bind each Decision to its decision Question. Record explicit applicability links from options and criteria to that Question and subject links from material challenges to the reviewed claim or decision. At review, persist each applicable built-in set rule, its version, scope and digest of sorted member IDs with substantive basis digests. Include empty sets. The core derives the required rules and checks them against current locked state. A changed membership yields a conflict and an audience-safe change description. Include set checks in replay and effective disposition after acceptance.
+
+Evaluate promoted bases against the final proposed state of an atomic batch as well as the ordered operations. Reject a batch that would publish a newly promoted claim on an already obsolete basis. Later independent transactions preserve historical acceptance and update effective freshness. One brief, including its links and pagination, uses one verified read snapshot.
+
 Concurrent assessments coexist as records. A disputed judgment is resolved by an explicit decision/review, not a wall-clock tiebreak. No automatic “last agent wins.” A stale mutation returns current revisions and a bounded diff; only an identical idempotent retry is automatic.
 
@@ -1944,4 +1951,6 @@
 - [ ] Concurrent independent additions succeed; competing substantive updates produce a clear CAS conflict.
 - [ ] A conclusion promotion or decision acceptance racing with evidence retraction/criterion change cannot validate against a stale read-set.
+- [ ] A new in-scope hard criterion or new material challenge after review conflicts before promotion and marks an already accepted basis `needs_review`. Moving or retiring the member also updates the set. Adding a criterion to another question in the same case leaves the reviewed set current.
+- [ ] A batch that validates and then adds a disqualifying blocker to the same claim fails atomically. Historical navigation uses the same head across linked reads. Full recomputation and indexed results agree after replay.
 - [ ] A malformed operation in a batch commits none of the batch.
 - [ ] Duplicate keys, unknown required fields, overlarge input and invalid references return bounded field errors.
````

</details>

### 3. Establish independence for each claim, while preserving every acquisition

**Priority** High impact, low effort in the model and fixtures. This changes what counts as evidence in all four domains.

Section 17.4 expects a primary document, trade article and blog to share one acquisition. Someone can collect all three separately. The article might repeat a value from the specification in its first paragraph and describe its own bench test in its second. Merging the acquisitions loses custody. Assigning the entire article to one independence group loses the bench test. Giving every URL a vote overstates support for the original value.

**Evidence and rationale** Cochrane links multiple reports of one study while retaining material unique to each, and recognizes that one report may describe multiple studies. W3C PROV distinguishes an activity from a quotation, revision or derivation. The transfer to technical investigation is to ask which originating observation supports *this particular claim*. Shared origin can be documented, suspected or unknown. [Cochrane Handbook §5.2.1](https://www.cochrane.org/authors/handbooks-and-manuals/handbook/current/chapter-05#section-5-2-1) · [W3C PROV-DM §5.2](https://www.w3.org/TR/prov-dm/#component2)

**Proposal and benefit** Keep the collected document, the quoted passage and the reported test separately addressable. The sources view can then say “three collected documents repeat one specification value” and link the separate test with its own setup limits. Distinguish an erratum that corrects the old edition from a later edition that changes the specification. Those warrant different answers to “what was correct for this build?”

**Implementation and cost** Reuse selector-bound Observations, Assessments and versioned relationships. A reviewer can mark overlap unknown and refine it later. The tool can group explicitly linked roots. Independence, a source's truthfulness and the applicability of a new edition still require judgment.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -448,4 +448,6 @@
 Record original acquisition, upstream source, collection run, method, fixture/environment, and known dependence groups. Show those dimensions in review. Exact byte deduplication is useful, but different bytes can still share one flawed origin.
 
+For a stated claim, record the origin of the observation or assertion separately from the activity that collected its document. One report may quote an earlier assertion at one selector and report a different measurement at another. Assessments may link multiple roots with an attributed dependence judgment (`shared`, `partly_shared`, `independent_on_stated_dimensions`, `unknown`) and basis. Treat an absent link as unknown. Display document/acquisition counts separately from known claim origins. Corrections to grouping update dependent judgments.
+
 Separate:
 
@@ -1687,5 +1689,5 @@
 **Question:** do three independent references actually corroborate a disputed technical specification?
 
-Seed a primary document, a trade article quoting it, a blog summarizing the article, and a newer first-party erratum. The agent must identify that the blog and article share *one original acquisition*, record the erratum's effect on currentness, and form a conditional conclusion (or explicit insufficient-evidence result) with direct source links.
+Collect a primary specification, a trade article, a blog and a first-party erratum as four separate Sources. For the disputed value, link the first three passages to one originating specification claim. Give the article a second passage reporting the author's own bench test. That passage has a different reported origin. If the underlying measurements are unavailable, preserve that limitation. Trace the erratum to the affected edition. In a twin fixture the changed value first applies to a later edition. The successor must answer for the build named in the question and retain every original selector.
 
 **Review/learning step:** conduct a blindspot review of source dependence, currency and missing first-party documentation; produce an explicit Retrospective stating why multiple downstream reports did not add independent support.
@@ -1875,5 +1877,5 @@
 - [ ] Case mode `evaluate` supports options/criteria/comparisons/decision with **zero hypotheses, zero tests and zero conclusions**.
 - [ ] Nothing in default `inv` help, on-disk default workspace, schema or binary requires a named external tool, target, permission, principal or security concept.
-- [ ] One source copied/summarized in three places remains one acquisition family, not three independent confirmations.
+- [ ] Three separately collected documents quoting the same specification preserve three acquisition records and one known origin for that claim. A second claim in the article may have a different or unknown origin. Reimport of the same acquisition remains idempotent.
 - [ ] Two distinct acquisitions with identical bytes remain distinct provenance identities while blob bytes deduplicate.
 - [ ] A producer's claimed result remains attributed source data, never auto-promoted to validated Conclusion.
````

</details>

### 4. Distinguish an unseen test from a fresh run over a known answer

**Priority** High impact, low to medium effort. Include the distinction before freezing InquiryPlan and Run.

An investigator sees the answer in a dashboard, freezes a prediction and runs a second parser over the underlying trace. A valid executor receipt can establish every ordering fact in §6.5. It cannot establish that the trace was new to the investigator. The useful outcome is that the second extraction agrees. Calling the prediction confirmed would credit the same observation twice.

**Evidence and rationale** The secondary-analysis preregistration template asks what each author already knew, including knowledge obtained from published summaries. It allows that previously collected data may still answer a prospectively specified question. Johari and colleagues show why stopping a fixed-sample test when its result looks favorable changes the inference. These are separate from clock ordering. [Van den Akker et al., prior knowledge and analysis specification](https://preregr.opens.science/articles/form_prereg2D_v1.html) · [Johari et al., sequential monitoring](https://arxiv.org/abs/1512.04922)

**Proposal and benefit** At confirmatory freeze, capture the investigator's declared exposure to the outcome, the evidence or holdout boundary, what will be measured and what ends collection. At assessment, compare actual inputs, exclusions and stopping with that plan. A deviation can be scientifically interesting and should remain easy to record. A later run over genuinely withheld units can address the revised question prospectively.

**Implementation and cost** Add a small conditional group to the existing freeze and assessment schemas. A held-out set can use a named batch, independent run or sealed manifest. A split by file is inadequate when both files contain the same requests. The tool can check recorded overlap and chronology, while the assessor owns the account of prior knowledge. A new actor, a fresh filename or a missing read log cannot establish blinding.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -536,5 +536,5 @@
 | Record observation | Source selector or explicitly labeled assertion, extraction/measurement metadata. | Is statement genuinely observed rather than inferred? |
 | Freeze exploratory InquiryPlan | Purpose, method or inspection, observation channel and bounded stop rule. | Is exploration useful? |
-| Freeze confirmatory InquiryPlan | Exact question/hypothesis revisions, predicted results, observation and control conditions, boundaries. | Does the proposed test actually discriminate? |
+| Freeze confirmatory InquiryPlan | Exact question/hypothesis revisions, predicted outcomes and contrast, observation and control conditions, intended evidence boundary, investigator-declared prior exposure, stop and analysis rule appropriate to the method. | Would a plausible alternative give a different outcome, and could the investigator already know this one? |
 | Record Run | Producer identity, plan or exploratory purpose, external reference, explicit status, honest completeness. | Were instruments valid? Was run comparable? |
 | Claim an informative absence | Expected signal, adequate opportunity/sampling, valid instrument/control or justified equivalent. | Are detection limits appropriate? |
@@ -556,10 +556,12 @@
 Because Investigation does not execute the test, it cannot prove the first external packet happened after a local record. Store chronology strength:
 
-- `retrospective`: plan or interpretation recorded after known outcomes, or ordering unknown;
+- `retrospective`: the plan was recorded after the target Run or inspection. Use `order_unknown` when the ordering cannot be established. Assess prior knowledge of data used in that Run separately.
 - `recorded_before_run_declaration`: local journal order only;
 - `executor_bound`: a supported executor receipt binds the plan digest before its execution;
-- `externally_attested`: a future verifiable external timestamp/checkpoint, with authority and verification details.
-
-Never upgrade chronology from timestamps alone. Exploration is valid; it becomes confirmation only through new, properly specified evidence.
+- `externally_attested`: independently verifiable evidence binds the plan and ordering of the relevant execution event. A timestamp over the plan alone dates that plan. Record the authority and verification scope.
+
+Record order, prior outcome exposure and reuse of evidence on separate axes. Chronology describes when the local declaration or receipt occurred. Record the investigator's attributed exposure to the target result (`unseen_declared`, `partly_seen`, `seen`, `unknown`), including earlier summaries. Record the observation units, partitions or future collection boundary reserved for the test and their relationship to the discovery data. A timestamp or executor receipt carries no blinding guarantee.
+
+At assessment, link actual input roots, outcome/contrast, exclusions, stop rule and material deviations to the frozen basis. A repeat extraction of the discovery observations may support reproducibility. Known outcome reuse cannot qualify as a fresh prospective test of that prediction. Unknown exposure or unestablished separation stays visible. A previously collected but genuinely held-out set can qualify within its declared limits. Preserve post hoc analyses with their actual status and propose a successor test when warranted.
 
 ### 6.6 Stop reasons
@@ -1888,4 +1890,6 @@
 - [ ] A valid control under the wrong build does not qualify the target treatment measurement.
 - [ ] A failed or blocked external Run cannot be interpreted as negative evidence of the question without a separate valid observation.
+- [ ] An executor-bound reanalysis of an already inspected trace retains its strong chronology and known outcome reuse. A renamed derivative counts as the same data. A separately held-out acquisition with a frozen plan remains distinguishable.
+- [ ] If a fixed-sample analysis stops early after a favorable look or changes the primary outcome, its result is recorded with the deviation. An explicitly planned sequential procedure is judged against its own rule.
 - [ ] A frozen predicted outcome cannot be silently amended after results to claim prospective success.
 - [ ] A credible counterhypothesis and material objection are visible before conclusion promotion.
````

</details>

### 5. Start from a real source in one write and recover the receipt without the source

**Priority** High impact, medium effort. Make this the Milestone A front door.

Sections 8.1–9.4 teach a newcomer to find a schema, construct an envelope, create a case, find an import example, supply a case ID and finally save the source. The first-use clock starts after discovery. Section 9.6 promises receipt lookup after an ambiguous write, while §9.1 lists no lookup command. The worst time to discover this is after a pipe has closed or a temporary source file has disappeared. Printing a generated request key *only in the lost response* cannot recover it.

**Evidence and rationale** The AWS Builders’ Library describes binding a caller-supplied identifier to one intent and rejecting incompatible reuse. Borrow that property for a local source stream. The workspace's CLI design convention also accepts the common `--json` spelling while documenting `--format json`. [AWS Builders’ Library on idempotent requests](https://d1.awsstatic.com/builderslibrary/pdfs/making-retries-safe-with-idempotent-apis-malcolm-featonby.pdf) · (the reviewer's local CLI design conventions)

There is also a real naming collision. Python Invoke registers `inv` for its task runner. Use the already planned long executable name for cold-start instructions and emitted commands, and preserve a user's existing short command on install. Neither name resolved on this review shell's PATH. [Invoke's published entry points](https://github.com/pyinvoke/invoke/blob/main/pyproject.toml#L45-L47)

**Proposed first use** With an existing `spec.txt`, these commands create a workspace, preserve the source and retrieve its receipt. The transcript specifies proposed behavior.

```sh
investigation init ./casework
investigation --workspace ./casework start 'What limit does this release document?' --attach - --command-id limit-first --format json < spec.txt
investigation --workspace ./casework receipt limit-first --format json
```

The second command returns the new case and artifact IDs, the digest, and one ready-to-run local read using those actual IDs. It records stdin as the supplied acquisition channel with unknown original publisher unless supplied. It makes no claim about what the file says. The third command is useful if the reply was lost. It takes no file or stdin. In an existing case, onboarding offers the ordinary scoped ingestion path.

**Benefit and cost** This turns the first action and its recovery into user-facing contracts. One small front-end form expands into the same staged ingest and domain transaction as the structured API. It introduces one atomic new-case import variant. Requiring the caller to retain a short command key keeps machine recovery unambiguous. Test both flag spellings, a malformed argument, an existing second case, a failed attachment, a lost reply and changed bytes under the same key.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -3,5 +3,5 @@
 **Status:** implementation specification, not a description of a working product.  
 **Revision:** 3, 2026-11-21 — Sapper/nvwabase-informed retrieval, blindspot review, and methodological learning; domain-neutral core preserved.  
-**Product:** `investigation`; CLI executable `inv` (alias `investigation`); reusable Rust library and optional MCP adapter.  
+**Product:** `investigation`, offline Rust CLI and reusable library with optional MCP adapter. Ship the long executable name. The short spelling `inv` in this specification denotes that executable. Offer the short command where it is available, preserving any existing command of the same name. Generate first-use docs, repair argv and integration defaults with `investigation` or the explicitly configured executable.
 **Scope:** general-purpose, agent-native investigative reasoning, evidence management, and decision support. No security-tool coupling.
 
@@ -655,12 +655,10 @@
 or a handoff. Skip for one-shot facts or ordinary implementation tasks.
 
-Start:   inv init ./casework
-Learn:   inv onboard --format json
-Resume:  inv brief --case <id> --format json
-Decide:  inv next --case <id> --format json
-Audit:   inv blindspots --case <id> --format json
-Inspect: inv show <id> --depth summary --format json
-Record:  inv schema observation.add --format json
-         inv record --input operation.json --format json
+Create a workspace and see the next available action.
+  investigation init ./casework
+  investigation --workspace ./casework onboard --format json
+
+Already inside a case workspace?
+  investigation brief --format json
 
 Tool output is a source claim, not a conclusion. `inv` records; it never
@@ -685,5 +683,5 @@
 `inv schema <operation>` must include an example by default. `--schema-only` returns just the JSON Schema. `inv schema --list` is a compact index grouped by workflow stage. An unknown operation suggests close matches and the catalog, without changing state.
 
-When a fresh workspace has no case, onboarding returns the exact `case.create` schema/example first. When an observation lacks source material, it returns the ingestion step first. The agent must not be sent straight to an observation payload whose prerequisite IDs do not exist.
+In an empty workspace, lead with one explicit new-case route. If the investigator already has a source, show the `start` form with a real example input and a caller-known command key. If they only have a question, show `case.create`. In a selected case, offer the existing scoped ingest or observation route. After each write return one local read using the actual workspace, case and created ID.
 
 Do not optimize solely for a small top-level command count. A generic `record` with an undiscoverable bag of operation names is poor UX. State-aware examples and errors are part of the API contract.
@@ -809,4 +807,6 @@
 |---|---|---|
 | `init PATH` | Create isolated casework workspace and optional vault. | Owned local files only. |
+| `start QUESTION --attach - --command-id KEY` | Explicitly create an exploratory case and preserve the supplied source stream in one transaction. | One case, Source, Artifact and receipt. |
+| `receipt KEY` | Resolve a canonical command receipt for the selected workspace and actor at a verified head. | None. |
 | `demo PATH --scenario NAME` | Generate reproducible tutorial cases (`debug`, `performance`, `research`, `choice`). | Local fixture only. |
 | `onboard` | Discover workflow and exact next valid command. | None. |
@@ -840,6 +840,6 @@
 - Read commands may select the only case automatically. Multiple cases with no explicit selection return `CASE_REQUIRED`, with a compact list. Do not guess from last UI focus.
 - `case.create` has `case_id: null`; all case-scoped mutations require one.
-- `--actor` selects an attributed actor; default can come from workspace-local configuration or the established `AGENT` convention. Never interpret this as verified identity.
-- `--format json` guarantees one JSON object on stdout. Human text is the TTY default; machine callers must pass the flag explicitly. Logs/progress use stderr and never contain raw artifacts by default.
+- `--actor` selects an attributed actor; default can come from workspace-local configuration or the established `AGENT` convention. Use the documented workspace-local unattributed identity when neither is set, including on receipt lookup. Display the chosen attribution. These values do not authenticate a person.
+- `--format json` and its global `--json` alias return the same JSON object on stdout for normal command, discovery and recoverable argument-error paths. Resolve format before subcommand validation. Keep progress on stderr. Use readable text for a terminal and plain, color-free output for pipes.
 - No ANSI, pager, prompt, or auto-open behavior in JSON mode. `--no-color` and `--no-pager` work in text mode.
 
@@ -881,5 +881,5 @@
 | Operation | Required meaning / fields |
 |---|---|
-| `case.create` | Title, objective, mode (`explore` default), question/decision context, scope, stop condition. Empty exclusions okay. `record` mode may be minimal. |
+| `case.create` | Title, objective, mode (`explore` default), question/decision context, scope, stop condition. At first capture in `explore`, scope and stop condition may be explicitly unknown. `record` mode may be minimal. |
 | `case.amend` / `case.pause` / `case.reopen` / `case.close` | CAS revision, attributed reason; closure outcome, unresolved questions, reopen condition; `case.close` in substantive modes must reference a prior/same-batch Retrospective. Mode change is explicit. |
 | `question.create` / `question.resolve` | Scoped answerable wording, consequence; evidence-backed answer or explicit inconclusive state. |
@@ -1009,5 +1009,5 @@
   "warnings": [],
   "next": [
-    { "kind": "local_read", "argv": ["inv", "schema", "hypothesis.create", "--format", "json"] }
+    { "kind": "local_read", "argv": ["investigation", "schema", "hypothesis.create", "--format", "json"] }
   ]
 }
@@ -1015,4 +1015,6 @@
 
 Hash placeholders in specification examples are notation, not accepted digest values. Production output returns real values. A retry returns the original transaction and created IDs with `result: already_committed`; current projection health may be reported separately.
+
+For stream or file capture, bind the semantic request hash to the caller's declared metadata and captured content digests in attachment order. Reusing a key to submit changed bytes is a mismatch, including when a path string is unchanged. For read-only reconciliation, `receipt KEY` requires neither the old request nor any source bytes. Return original IDs, revisions, snapshot and artifact digests from canonical-derived state, with current projection health labeled separately. An absent key is `not_found_at_snapshot`. A request might still be in flight. An unverifiable head yields unknown status and a repair path. Keep caller-selected keys available before submission across CLI, MCP and producer wrappers.
 
 ### 9.6 Error envelope and exits
@@ -1158,5 +1160,7 @@
 ### 11.1 `ingest` contract
 
-`inv ingest --input manifest.json --format json` accepts a strict manifest with `schema_version`, `command_id`, `case_id`, `actor`, `source`, `content`/`paths`/`external_refs`, extraction options and sensitivity policy. `inv schema ingest` returns a working local-file example plus exact behavior. Producer-provided metadata is not an executable instruction.
+`inv ingest --input manifest.json --format json` accepts a strict manifest with `schema_version`, `command_id`, exactly one of `case_id` or `new_case`, `actor`, `source`, `content`/`paths`/`external_refs`, extraction options and sensitivity policy. `inv schema ingest` returns a working local-file example plus exact behavior. Producer-provided metadata is not an executable instruction.
+
+Add a mutually exclusive `new_case` variant alongside the existing `case_id` variant of the ingest manifest. The variant binds the new case and its supplied sources internally within the same transaction. The CLI `start` form translates to this variant with a question, mode `explore` and one stdin attachment. Accept explicit unknown scope and stop condition during initial capture, then suggest framing when needed for promotion. The question supplies the initial objective/title. Record actor, acquisition channel, size and measured digest. A failed import publishes neither case nor Source. Reject a conflicting existing case selector. Keep direct path imports subject to the configured roots and limits.
 
 Ingestion steps:
@@ -1941,4 +1945,5 @@
 - [ ] Kill the writer at every publication boundary in §10.4 and recover honestly.
 - [ ] Lose the response after commit; identical retry returns the original IDs exactly once.
+- [ ] Pipe the first source into a new case, lose the response and discard the input. A read-only receipt lookup after restart/reindex gives the same case, artifact and digest. A changed stream under the same key is rejected. Failure during initial ingest creates no empty case.
 - [ ] Same command ID with changed payload is rejected, including after index rebuild.
 - [ ] Concurrent independent additions succeed; competing substantive updates produce a clear CAS conflict.
````

</details>

### 6. Explain what the next observation would change, and let the investigator correct the advice

**Priority** High impact, medium effort. Pilot one explanatory and one decision case.

Section 7 already ranks three actions and can label one `could_flip_choice`. It does not require an answer to “which choice, and how?” The example in §8.5 directs a user with an unmeasured hard criterion to `review.submit`. Another review cannot supply the missing measurement. Section 7.2 also filters explicit deferrals, but the operation catalog gives the user no clear way to defer one recurring suggestion and later restore it.

**Evidence and rationale** NASA asks whether reducing a particular uncertainty can change the choice and justify the cost. Amershi et al. identify efficient dismissal and correction as interaction requirements. Applying these principles to a deterministic advisor is a design hypothesis. Its value can be measured by whether a successor chooses a discriminating step or stops at the right point. [NASA, Decision Analysis §6.8.1.2.4–5](https://www.nasa.gov/reference/6-8-decision-analysis/) · [Amershi et al., CHI 2019, guidelines 8–9](https://www.microsoft.com/en-us/research/wp-content/uploads/2019/01/Guidelines-for-Human-AI-Interaction-camera-ready.pdf)

**Proposed experience** In the queue case, the first card might say “Recovery during restart is the only unresolved must-have for A. A pass keeps A eligible. A fail rules it out under the current requirement. A partial test leaves eligibility unresolved.” That card can point to an existing Run and its evidence or ask for the missing observation. Show who supplied those consequences or which typed rule derives them. In a debugging case where both candidate causes predict the same slow request, repeating it may establish reproducibility but cannot distinguish those causes. If the record supplies no outcome branches, say what information is missing.

**Benefit and trade-off** One leading card, alternatives on demand, and a reversible “source unavailable” disposition give the user control over repeated advice. The price is tracking when a disposition expires. Reuse a scoped Annotation and the existing frontier. Deferring a suggestion leaves its underlying blocker and uncertainty visible in a decision review. Test the overview in a narrow, monochrome terminal and the casebook by keyboard with a linear reading order. [W3C guidance on use of color](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html) · [W3C guidance on keyboard operation](https://www.w3.org/WAI/WCAG22/Understanding/keyboard.html)

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -612,4 +612,6 @@
 Expose each factor and its source. This is a **heuristic**, not a calculated expected information gain or probability. A decision comparison may support user-supplied weights, but the default does not declare a winner from arbitrary score sums. Sensitivity checks should ask whether plausible uncertainty could reverse the choice, not create false precision.
 
+Show one leading recommendation in compact human output, with up to two alternatives available. For decision-changing or discriminating advice, show the actual question, the missing observation, and how distinct possible outcomes would change eligibility, evidentiary standing or the choice. Use recorded outcome branches or deterministic consequences of a typed threshold. Label an investigator-supplied forecast with its source. If the plausible branches lead to the same disposition, identify any narrower value such as replication and rank the step accordingly. If branch effects are unknown, request that information or show the uncertainty. When feasible steps cannot affect the declared decision within its limits, include a justified stop or defer option.
+
 Return at most three recommendations by default, plus counts of blocked/deferred actions, with a pagination/query path. `next` is read-only; claims and run execution must be separate explicit operations.
 
@@ -640,4 +642,6 @@
 
 `inv frontier --case C --format json` is a read-only **complete, filterable inventory of investigable gaps** at a committed snapshot, while `inv next` provides a ranked top three. It distinguishes `ready_to_analyze`, `requires_source`, `requires_external_action`, `blocked_by_quality`, `stale_due_to_revision`, `deferred`, and `resolved`; each item includes the exact subject/read-set, source of the gap, reason for status, prerequisites and a safe `schema`/`show` route. Support `--kind`, `--status`, `--limit`, snapshot-bound pagination and `--explain GAP_ID` for the deterministic rule that emitted it.
+
+Support a case-scoped `annotation.add` with `role: advice_feedback`, the subject, built-in rule/version, target IDs, material basis, disposition (`defer` or `resume`), reason and optional revisit condition. Derive the advice identity from rule, scope and targets. Reevaluate applicability on material change or explicit resumption. Unrelated appends preserve the deferral. Keep deferred items discoverable in `frontier`. Feedback only changes advice presentation. Validation, caveats and the underlying gap continue to use canonical evidence and policy.
 
 A frontier item is **not** an external task assignment, an instruction to execute a test, proof that the suggested method is good, or a claim that all missing alternatives were discovered. External work systems may translate frontier items into tasks, but `inv` does not create or dispatch workers. Mark structural rule IDs/versions in the response so a changed rule is not mistaken for newly acquired evidence.
@@ -759,5 +763,5 @@
       "subject_refs": [{"id": "<returned-criterion-id>", "revision": 2}],
       "why": "Hard requirement has unknown evidence-backed disposition",
-      "suggested_schema": "review.submit"
+      "suggested_schema": "comparison.add"
     }
   ],
@@ -889,5 +893,5 @@
 | `comparison.add` / `comparison.revise` / `comparison.retract` | Exact option+criterion revisions, result/value (`unknown` allowed), evidence/assumptions, method, uncertainty and rationale. |
 | `assumption.create` / `assumption.assess` | Premise, materiality, affected subject links, disposition, evidence/reasons. |
-| `annotation.add` | Subject refs, attributed freeform content or note snapshot and classification; optional `role: synopsis` requires exact subject revision and is separately stale-able. |
+| `annotation.add` | Subject refs, attributed freeform content or note snapshot and classification; `role: advice_feedback` records a reversible rule/subject disposition (§7.5); optional `role: synopsis` requires exact subject revision and is separately stale-able. |
 | `source.assess` / `source.withdraw` / `artifact.withdraw` | Source authenticity/reliability with limitations; withdrawal/reason/scope; preserved immutable originals. |
 | `observation.add` / `observation.correct` / `observation.retract` | Narrow measured/reported value, selector, units/method, observation quality/unknowns, immutable revision history. |
@@ -1130,4 +1134,6 @@
 Define measured targets on a documented ordinary workstation: warm brief/next under 250 ms p95 for 10,000 objects and 50,000 relations; warm mutation under 150 ms p95 excluding file ingestion/render; streaming artifact import with bounded memory. These are engineering targets, not measured current performance. If the first implementation misses them, profile before adding complexity.
 
+Include a sparse choice case with 1,000 options and 200 applicable criteria. That is 200,000 possible cells even with few recorded Comparisons. Group repeated gaps by question and criterion in `next`, provide snapshot-bound drill-down, and measure first-page and top-three latency including rule evaluation at the §10.5 target. Keep an exhaustive small reference fixture to check grouping and completeness. Materialize or cache a large cross-product only if profiling justifies it.
+
 ### 10.6 Integrity and authenticity
 
@@ -2035,4 +2041,6 @@
 7. Switch between synopsis, full reasoning and an exact evidence excerpt; identify whether the synopsis is stale.
 8. Locate the most recent blindspot challenge and methodological Retrospective; tell which lessons are speculative versus supported.
+
+Also ask the reviewer to decide whether a suggested measurement would change the current choice, defer an unavailable source, and find the still-visible blocker. Run the evidence-to-caveat path using keyboard navigation and a linear screen-reader or plain-text rendering, including a narrow viewport with color off. Preserve meaningful labels for status, uncertainty, links and next action. Count wrong turns and requests for help.
 
 Pass only when the reviewer can do these without a narrated tour. Capture UI observations and fix misleading status labels, overwhelming tables, or dead-end navigation. A screenshot looking polished is insufficient.
````

</details>

### 7. Specify which attempts a metric describes and how small an effect would matter

**Priority** High impact, low effort for the performance pack and fixtures.

A working instrument and a large sample can precisely answer the wrong question. Suppose 100 requests start. Ninety-five finish in 10 ms and five remain unfinished at a 1 s timeout. Every completion in the log took 10 ms, so its nearest-rank p99 is 10 ms. Only 95/100 met a requirement to finish within 25 ms. The exact all-attempt completion-time p99 is unavailable from these observations. Calling the missing five “no data” in an appendix would change the decision.

**Evidence and rationale** Google's SRE workbook distinguishes the service outcome from the implementation that measures it, including requests absent from server logs. Lakens describes how practical equivalence requires justified bounds and a compatible test. A functioning monitor and a nonsignificant difference leave that question open. Likewise, review 2's 299-sample figure follows from one particular bound. For independent identically distributed continuous samples the probability that the sample maximum covers the population p99 is `1 − 0.99^n`, about 95.05% at 299. It supplies neither a universal p99 sample floor nor a solution to missing slow requests. [Google SRE Workbook, Implementing SLOs](https://sre.google/workbook/implementing-slos/) · [Lakens, Equivalence Tests](https://pure.tue.nl/ws/portalfiles/portal/80918653/lakeequi2017.pdf)

**Proposal and benefit** On promotion of a quantitative comparison, state the target population, unit, aggregation, timeout/exclusion rule, and independent replication unit. Ask “what difference would change the decision?” before looking at results when practical. In a separate mean-difference fixture with a prespecified equivalence band of ±5 ms, an externally validated, compatible interval of [−2,+3] lies inside the band. [−8,+3] does not. The second result is inconclusive about equivalence. Neither interval establishes exact equality.

**Implementation and cost** Put the extra fields in the performance pack and leave capture permissive. The producer supplies counts, intervals and method provenance. The core can check matched definitions, denominator arithmetic and presence of a prespecified margin. Verifying sampling assumptions or the appropriateness of a statistical method remains review work. This adds a more useful repair than asking the user to “collect more data”.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -1667,5 +1667,7 @@
 Agent tasks: record runs from an entirely generic producer manifest, assess instrument validity and comparability, distinguish process exit from meaningful measurement, identify a variance/correlation caveat, freeze a confirmatory repeat plan, decide whether effect is repeatable and bound the conclusion to the workload. Preserve both positive and negative trials. The blindspot review flags missing comparability/monitoring, and the Retrospective records which benchmark conditions wasted effort and what must be controlled next time.
 
-**Negative variant:** 'no improvement detected' with broken performance monitor or insufficient observation window cannot become evidence of equality. A legitimate zero-effect result needs adequate measurement conditions and precise detection/power limits; the system cannot fabricate a p-value.
+**Missing-outcome variant.** Seed 100 started requests. Ninety-five finish in 10 ms and five remain unfinished at the 1 s timeout. Preserve the original counts and censoring information. Compare a completion-only latency statistic with the all-attempt requirement of 99% finishing within 25 ms. The latter has 95 successes out of 100. Its shortfall is visible even though the exact completion-time p99 for all attempts is unavailable.
+
+**Practical-equivalence variant.** Capture target population, outcome/units, contrast, aggregation, independent sampling or replication unit, missing/timeout policy and any decision-relevant margin on the frozen inquiry. A quantitative Comparison identifies which definition its evidence implements. For a practical-equivalence claim require a prespecified band with rationale and a compatible reviewed external analysis that excludes meaningful differences under its assumptions. Distinguish improvement, deterioration, practical equivalence and unresolved effects. A broken monitor, an imprecise null result or a favorable post hoc choice of margin cannot establish equivalence.
 
 ### 17.3 Scenario C — architecture choice (evaluative/trade study)
@@ -1901,4 +1903,6 @@
 - [ ] Repeating a benchmark creates a distinct Run with an explicit acquisition identity and shared setup dependencies.
 - [ ] No p-value, confidence interval, causal claim or numeric accuracy score is invented by the core.
+- [ ] Preserve started, observed, timed-out, excluded and unknown counts with their definitions and overlap rules. An all-attempt criterion cannot be satisfied from a completion-only percentile. Missing denominators can still be recorded as unknown.
+- [ ] For a separate mean-difference test with prespecified bounds of ±5 ms and a compatible externally supplied equivalence interval, [−2,+3] supports bounded equivalence and [−8,+3] leaves it unresolved. The core invents neither an interval nor a probability.
 
 **Evaluative/architecture**
````

</details>

### 8. Give a producer a durable position and a precise end to its recording

**Priority** High impact, medium effort. Specify the race before implementing W07.

A recorded run is sending a control result while another client finalizes it. Event IDs in §11.3 prevent identical appends from happening twice. They do not say whether the distinct control arrived before the manifest closed, or whether an expected intermediate event never arrived. After staging is retired, clients still need a way to reconcile a lost acknowledgment.

**Evidence and rationale** The tus protocol makes a client state its expected upload offset and rejects a mismatch without advancing the stream. Its position is about bytes. For `inv`, the analogous unit is an accepted producer item or attachment descriptor. Arbitrary samples can still live inside one artifact. A producer-specified expected end establishes completeness only for the declared stream. [tus resumable upload protocol, core protocol](https://tus.io/protocols/resumable-upload#core-protocol)

**Proposal and benefit** Serialize append, attach and seal per handle. Persist the accepted cursor, item identity and digest before acknowledging. Give finalization the expected end and artifact set. An identical retry gets its original receipt, even after sealing. A late distinct item gets a closed-handle response. An interrupted run can still close with partial evidence. This gives a successor both an honest completeness judgment and a usable recovery position.

**Implementation and cost** Bound memory per request and bytes per segment. Report a full spool while preserving acknowledged items. Archive the sealed item map as immutable artifact bytes referenced by the canonical finalization event. Avoid a global transaction per high-frequency sample. After recovery a racing append must either be in the seal with a retrievable receipt or have a recoverable closed-handle result. Test disk-full and a crash between sealing and canonical publication.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -1210,4 +1210,8 @@
 Never hold the canonical commit lock while an external process runs. Stage outputs durably, then snapshot only stable bytes: require producer writer close/flush or a safe copy-and-verify protocol. Prevent writable hardlinks into canonical blobs. Every unknown/interrupted status stays explicit. A producer crash may occur after effects but before finalization; recovery must not assume the operation was never performed. Distinguish `finalized` versus `execution_completed`.
 
+Every accepted append or attachment descriptor has a per-handle position and content/request digest. Accept only the next expected position under per-handle serialization. Check an already-seen item key first: identical retries recover the original receipt, and mismatched content conflicts. Durably persist bytes, item receipt and cursor before acknowledgment. `capture recover` returns the accepted position and seal state.
+
+Finalize under the same serialization with an expected last position and declared artifact/channel inventory. Seal staging before publishing the canonical manifest. Reject a stale cursor for a claimed complete stream. When an intermediate item is lost, allow partial finalization with the missing position identified and a surviving out-of-order tail attached separately with its original positions. Unknown expected extent stays unknown. Record completeness relative to the producer declaration. A new item arriving after sealing receives a closed-handle response. Preserve late evidence through explicit linked reconciliation. After loss of power, finish or inspect the same seal. The finalized manifest binds a durable item-key, digest and receipt map sufficient for replay after staging is retired.
+
 **Producer readiness:** if durable recording was required by the producer policy but capture initialization fails, the producer must not start a new effectful unit. If recorder fails midrun, preserve a bounded local spool and reconcile when possible, while the producer handles its own safe cleanup. Those are producer obligations, not commands the `inv` core executes.
 
@@ -1964,4 +1968,5 @@
 - [ ] Capture finalization after process failure preserves exact original error and valid partial observations; no invented overall verdict.
 - [ ] Historical or external manifest reimport is idempotent; manifest revision isn't a new trial.
+- [ ] Send position 3 when 2 is missing, retry an accepted item with changed bytes, race the last attachment with sealing, and interrupt between seal and canonical publication. Each acknowledged item survives recovery at its recorded position. After archive/restore and staging retirement, an identical retry recovers its original receipt, and a new late item cannot silently alter the manifest.
 - [ ] Unknown source schema or namespaced metadata stays opaque with usable diagnostic; unsupported source data never silently maps to false facts.
 - [ ] Core compilation/run tests pass with zero installed producers, executors, task trackers, or environment credentials.
````

</details>

### 9. Keep a restricted objection from turning into an unqualified public recommendation

**Priority** High impact, medium effort. Define the audience contract in W06 and test each output in W08/W11.

Suppose a decision and its favorable benchmark are public, but the only evidence that recovery failed is restricted. Section 8.3 requires the strongest caveat even under a tight byte budget. Sections 15.4 and 16.3 require positive inclusion, omission details, digests and an exact snapshot. Removing restricted nodes before computing the review state can make the public recommendation look stronger. Including their names, digests, counts or the global sequence can reveal protected information.

**Evidence and rationale** W3C's provenance access note recognizes that even the existence of a referenced resource may be sensitive. The disclosure rule proposed here follows from the plan's own promise to preserve material caveats across views. [W3C PROV-AQ, direct query and security considerations](https://www.w3.org/TR/prov-aq/)

**Proposal and benefit** Compute the effective disposition and material caveats at the full snapshot. Then decide if a truthful representation of that answer and its caveats is approved for the audience. An approved abstract caveat can suffice. Otherwise withhold the dependent recommendation or decline the requested report. Give the publisher a private explanation. Where even existence is protected, the distributable report gets no identifying placeholder.

**Implementation and cost** Render an audience-specific bundle into a fresh destination. Keep the bundle's own digest and approved identifiers in it, with a private mapping to the workspace head, policy and omissions. Some provenance will be traceable only by an authorized reviewer. Test reports, brief, MCP, search, pagination, attachments and Canvas against the same materiality decision. A historical head must obey current disclosure policy.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -1587,5 +1587,5 @@
 All reports render deterministically from exact committed snapshots, no model call required. A synopsis, blindspot list or lesson cannot be substituted for primary evidence. Reports link to exact reviewed states and annotate changed/withdrawn sources and stale review bases. Allow agents/humans to improve authored summaries via typed amendments, not by silently rewriting canonical claims in a renderer. Negative and inconclusive results remain visible. Mark deprecated/superseded decisions instead of rewriting the past.
 
-Audience policy is positive inclusion, not a best-effort scrub. Review vault and recovery archive have intentionally different completeness/security guarantees. Export manifests contain snapshot ID, renderer/schema versions, source digests, omissions, classification policy and where applicable supersession links. Optional HTML stays inert and self-contained; PDF is an optional later exporter.
+Export from a committed snapshot using a positive inclusion policy. Keep an internal manifest of source digests, workspace head, renderer/schema versions, policy revision, omissions and supersession links. An audience-facing manifest identifies the approved bundle and permitted provenance. Record the mapping to the canonical snapshot privately when a global head or sequence would expose other activity. Retain the complete snapshot identity in an authorized recovery archive. Optional HTML stays inert and self-contained. PDF is a later exporter.
 
 ## 16. Trust boundaries and retention
@@ -1613,5 +1613,7 @@
 Export uses a positive inclusion manifest and audience policy. Check every channel: body text, labels, aliases, filenames, source URLs, Canvas cards, generated summaries, human annotations, errors/logs, attachment metadata, and attachments. Default review bundles exclude raw canonical events, local absolute paths, credential sources, and raw captures.
 
-A redacted review vault must not quietly contain the unredacted canonical journal needed for resumption. Review export and full recovery archive are different products. Mark completeness, omissions, audience, snapshot and policy version in the manifest. If requested material cannot be included under the chosen policy, fail with exact exclusions or generate an explicitly incomplete review bundle—never label it complete.
+Compute effective state and material support/objections against the full canonical snapshot before audience filtering. A shared Conclusion or Decision must carry an approved representation of its operative caveats. If even a general caveat would mislead or disclose protected facts, omit the dependent statement or refuse that report. Use the same decision for previews, summaries, search, counts and renderers. Source names, hashes, IDs and even the existence of an omission require audience permission. Put detailed diagnostics in a private publisher receipt. The public view may include an approved limitations notice.
+
+Build a review export in a fresh destination from allowed inputs. Ordinary working-vault conflict preservation can retain obsolete or edited sensitive material. Generate the distributable files independently of that directory. Bind public navigation, cursors and manifests to the published view where disclosing a global head is disallowed. Evaluate disclosure with current policy when regenerating a historical view.
 
 The tool exports locally. Publishing, emailing, cloud upload, or Git push requires a separately requested integration action. No automatic telemetry of case content.
@@ -1985,4 +1987,5 @@
 - [ ] Hostile HTML, remote embeds, plugin fences, YAML values and URI text stay inert in previews.
 - [ ] Review export excludes raw journal/secrets and checks labels/filenames/Canvas/annotations as well as artifact bodies.
+- [ ] A public decision with a restricted decisive objection is withheld or explicitly qualified by an approved caveat in every public channel. Verify the private disposition remains accurate. Test names, counts, hashes, cursor metadata, stale generated files and the omission manifest. An unrelated restricted append has no visible effect when policy hides its existence. Re-exporting an old snapshot respects today's policy.
 - [ ] A mobile/offline reader can understand the exported case without shell or community plugins.
 - [ ] Plugin setup/uninstall preserves unrelated configuration and custom configuration-directory settings.
````

</details>

### 10. Expire a case's access without deleting another case's only copy

**Priority** High impact when retention is enabled, medium effort. Specify the rule with the store and implement it with retention and restore.

Sections 10.1 and 11.4 deduplicate bytes across a workspace while preserving acquisition identities. Section 16.5 lets a case retention policy delete a blob. If cases A and B retained the same bytes and only A has expired, one physical unlink can destroy B's only evidence. An archive copying an earlier snapshot is another live reader. A digest reference in an old event also survives intentional removal, so testing for *any* reference can make reclamation impossible forever.

**Evidence and rationale** Restic separates forgetting a snapshot from reclaiming content still used by other snapshots. SQLite documents that FTS5 can retain old index entries until a merge. These are concrete reasons to distinguish logical availability, physical storage and copies in derived stores. [Restic snapshot removal and pruning](https://restic.readthedocs.io/en/stable/060_forget.html) · [SQLite FTS5 secure-delete behavior](https://www.sqlite.org/fts5.html#the_secure_delete_configuration_option)

**Proposal and benefit** Record expiry on the acquisition or artifact reference. Prevent reads of the bytes via that expired reference, including historical reads. Keep bytes while an eligible reference, durable interrupted capture or active archive still needs them. The preview should distinguish loss of access here, bytes retained elsewhere and bytes eligible to reclaim, subject to the caller's audience. This preserves B and gives A a truthful account.

**Implementation and cost** Commit expiry before reclaiming bytes, recheck reachability under writer/retention coordination, and make physical reclamation retryable. A snapshot pin or a lock shared by archive and reclamation suffices at first. Time the scan on the 10,000-object fixture and compare any indexed optimization with a full reachability walk. Existing backups, event text and filesystem remnants limit physical erasure claims. If v1 cannot qualify reclamation, ship withdrawal and truthful availability reporting first.

<details>
<summary>Exact changes to the plan</summary>

````diff
diff --git a/notes/plan.md b/notes/plan.md
--- a/notes/plan.md
+++ b/notes/plan.md
@@ -1142,4 +1142,6 @@
 A complete recovery archive must account for active/unreconciled staging. Initially refuse/defer a complete archive until those runs are finalized or explicitly recovered as interrupted; a later consistent staging-snapshot protocol may include them. A review-only export may omit staging with a clear limitation. Never silently drop recoverable staged evidence or sweep it through temporary-file GC.
 
+Pin the chosen recovery head and its required blobs until the archive verifies and publishes or the operation is explicitly abandoned. Serialize reclamation against pin creation and release. Ordinary later appends may continue. Apply the existing staging rule at the selected head. Snapshot included human notes as separate attributed inputs because they can change outside the journal.
+
 Archive restores go into an empty new destination and verify before publication. Preserve original IDs for an exact restore; a deliberate fork gets a new workspace identity and explicit origin snapshot. Do not let two forked workspaces masquerade as one coordinated writer.
 
@@ -1627,5 +1629,7 @@
 Ordinary withdrawal/retraction changes evidentiary use while preserving history. It is not erasure. State that clearly in commands and docs.
 
-Retain raw evidence according to a case policy. A deliberate retention operation may delete a raw blob and leave digest/size/provenance plus `availability: intentionally_removed`; all dependent claims/exports must reflect lost inspectability. Produce a preview of affected records and backups/exports known to this workspace before applying. Never silently GC a referenced artifact.
+Apply case retention to the individual acquisition or Artifact reference first. Commit its loss of availability before scheduling physical reclamation. Current policy and availability govern every byte read, including at a historical snapshot. A second acquisition of identical bytes retains its own policy and inspectability. Physical deduplication confers no cross-case read permission.
+
+Before unlinking a digest, recheck all live retention obligations, durable staging, and archive pins under coordination with import and archive creation. Historical references explicitly marked expired remain auditable without demanding the bytes indefinitely. Preview and report separately the logical loss on each permitted reference, physical bytes still held, reclaimable bytes and known downstream copies. On interruption, resume from the committed state. Never expose an expired reference just because another case retains the same digest. Include FTS, SQLite/WAL, old renders, previews and exports in the exposure inventory.
 
 If a secret was committed in event text, adding a redaction event does not remove the old plaintext. A dedicated purge workflow must create a sanitized new storage epoch/archive, preserve a redaction audit/mapping without the secret, and explicitly state which historical replay guarantees changed. Until that workflow is implemented, diagnose the exposure and document manual/backup remediation; do not claim secure deletion or an effective purge. Distributed copies cannot be recalled automatically.
@@ -1953,4 +1957,5 @@
 - [ ] Missing expected artifacts remain missing/incomplete, not replaced by empty files.
 - [ ] A full archive restored on a clean machine yields the same case state and retained evidence digests.
+- [ ] Import identical bytes into A and B, expire A during an archive of B, then read, restore and reindex. A's reference is unavailable even at an old head on the live workspace. B and the pinned archive verify. Reclamation proceeds only after the last obligation and pin release, survives interruption, and makes no claim about external copies.
 - [ ] Killing a process between a Retrospective and its same-batch `case.close` never produces a closed case with no accepted Retrospective, nor an orphaned half-transaction.
 
````

</details>

## Implementation decision

At Milestone A, ask an unfamiliar investigator to bring one local source and hand a case to a successor. Insert a new hard criterion between review and acceptance. Ask the successor what changed and whether another measurement would affect the choice. This single exercise puts the everyday interface, review freshness and investigative judgment under the same test. Use the results to decide how much of the catalog earns a place on the first screen.

**Review scope and verification** This review covers the 2,264-line revision 3 plan, SHA-256 `4048bc54435855815b535d0179468e0ae75c2f9c2a222b00c0f346470b5b6d1d`. Primary research, standards and maintainers’ documentation are linked with the findings they inform. The numerical examples are calculations or synthetic fixtures. The ten proposed diffs were checked individually against that revision and applied together in order to a temporary copy. The checkout contains planning documents. Runtime behavior, user performance, storage durability and accessibility still require the named trials on an implementation.
