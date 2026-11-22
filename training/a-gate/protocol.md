# A-gate paired pilot protocol

The acceptance rule is [ADR 0003](../../docs/adr/0003-a-gate.md). The independent unit is the underlying case family. Twin variants and two agent backends are repeated observations inside that family. Debugging and architecture choice each have at least three independent substantive families. A separate lookup family measures first-use burden where a tool may be unnecessary.

## Assignment and budgets

Build and retain a single Milestone A executable before the pilot. Record the Git revision, executable digest, runner version, provider and exact model ID, inference settings, kernel and tool versions. For each family, model and variant, assign both arms. Use a seeded schedule to balance notes-first and tool-first within each model and domain. Enforce pair order for a given variant and model. Assign fresh process and session IDs to each stage. A model gets no earlier variant, arm, sibling transcript, project checkout, onboarding hint, case key or grader output.

Both arms get the same shell and file commands, source bytes, empty `NOTES.md`, stage instructions, and budgets. The treatment adds the pinned `investigation` executable to the command path. Both sides can use plain notes, quote evidence, and use shell and search normally. Participants discover any executable and its help themselves. For each investigator allow 210 seconds and up to 28 shell calls, including the interval after review. For each successor allow 120 seconds and up to 14 calls. Each shell call gets at most 12 seconds and 12,000 UTF-8 bytes of output. Charge all model input and generated output, including cached tokens and reasoning where reported. The investigator and successor respectively have total metered input limits of 180,000 and 95,000 tokens, and generated output limits of 14,000 and 6,000 tokens. Check input limits before starting another model turn, and cap generated output by the remaining allowance. Record any provider-reported input overage on the last request, terminate the phase at that response, and retain the work.

## Interruption and evidence intake

At start the investigator sees the objective and a single local source. They can use the normal tools to inspect the source, retain material, and review a provisional disposition. They signal the review checkpoint with `review.ready`, after making their reasoning available to a reviewer in persistent state. The harness then atomically adds the preregistered second source, with the same bytes and filename for both arms. The architecture update introduces a new hard criterion between review and acceptance. The debugging update corrects or qualifies an observation on which the review depended. Injection changes the evidence directory only. There is no evaluator message explaining what the source means.

The investigator may read the new source, amend the record or notes, or leave an unresolved question. They write `handoff.ready` when finished. The runner interrupts the investigator at that marker or the first exhausted budget, whichever comes first. If review never happened, the runner stages the second source at interruption for the successor, and records the failed investigator checkpoint separately. The successor receives the same work directory, objective, and generic format instruction in a new session. The runner consumes readiness files before the successor starts. The investigator's transcript, answer, scratch outside the work directory and tool state are absent. The successor has an independent deadline, uses the same tools, and submits one answer.

The lookup family has no review update. It still uses the same time and call limits, both arms, and fresh handoff. Report lookup separately from substantive family estimates.

## Answer and evidence

The same four fields are required in both arms.

1. **Conclusion or choice.** State the current answer and whether it is validated, conditional, inconclusive, or a direct lookup. Explain what changed since review if relevant.
2. **Decisive citation.** Give the source filename and an exact line/range, JSON pointer, or distinctive verbatim excerpt.
3. **Strongest limitation.** State the unresolved threat to the answer or choice.
4. **What would change it.** State the next observation and what result would change the conclusion or choice. Say when an extra measurement would leave the choice unchanged.

The runner captures raw per-phase transcripts, boundary events, costs, status, and retained workspaces. Grading receives only normalized answer fields and seed-document selectors under random response IDs in shuffled order. Detect false promotions and missed warranted answers using the opportunities frozen per variant. Score review, rival quality, checkable retrospectives, capture and receipt mechanics separately. Preserve timeout and crash assignments in paired denominators. A missing answer misses all warranted rubric items. False promotions are actual assertions with excess warrant, so absence of an answer does not itself count as a promotion. Show both outcomes alongside failure counts and use the ADR's two directional criteria.

The evaluator commits the scheduling code, generic prompts, and rubric digests before the scored runs. Keep key material and the assignment-to-arm map outside the participant filesystem. Each participant tool invocation mounts a private work directory and a read-only operating system in a network-isolated namespace. Run the model transport outside that namespace with its normal credentials. Do not pass those credentials or external directories to participant processes.

## Development rehearsal

For a development timeout assignment, both investigators receive the same trace text and objective in separate empty directories. Either can inspect and quote the trace, then write a provisional review and create `review.ready`. The same clock qualification arrives in each directory at that marker. The first sessions end at `handoff.ready` or their deadlines. Each fresh successor receives only its arm's persisted sources and work. It answers the same four fields about the mechanism, citation, clock caveat, and a useful measurement. The model sees the same prompts, time and token limits in both runs. One arm can retain a verified excerpt and read a CLI handoff by using the added binary. The other can retain the excerpt and handoff in an ordinary Markdown file. The runner checks assignment and phase IDs, injection digests, both isolated boundaries, and availability of the baseline answer.
