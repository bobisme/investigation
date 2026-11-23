# ADR 0006 – Catalog disposition after the A-gate pilot

Accepted after analysis of the frozen A-gate pilot.

ADR 0003 calls for a strict improvement in either successor accuracy or false promotions. Accuracy improved by 12.5 percentage points at equal family weights. False promotions tied. The recorded rule therefore permits continuing with the catalog and preregistering the sealed release trial. The pilot measures access to the CLI. It gives little evidence about the effects of individual record types. [Pilot result](../a-gate.md) · [Precommitted rule](0003-a-gate.md)

Retain the current record types. The reports mark sparse use by opportunity so it remains visible to the release trial.

| Record types offered | Pilot evidence and disposition |
| --- | --- |
| Case | Three Case records across two substantive treatment assignments. Keep it as the unit of isolation, snapshot selection and handoff. |
| Source, Artifact, Observation | No typed instance at treatment handoff. Retain source identity, original bytes and a separate attributed observation so each supported claim has an exact trace. |
| Hypothesis, Inquiry, Run, Conclusion | No typed instance at treatment handoff. Retain the distinction between a rival explanation, test intent, test execution and bounded answer. The debugging proof exercises that separation. |
| Option, Criterion, Comparison, Assumption, Decision | No typed instance at treatment handoff. Retain the distinct identities and revisions so a hard requirement or an assumption can change while earlier judgments stay readable. |
| Review | No canonical Review predated the update in the treatment assignments. Retain the review boundary and exact basis. The worked architecture proof exercises stale review rejection independently. |
| Retrospective | No typed instance in this review-and-handoff task. Retain material closure and the explicit waiver path demonstrated by the worked cases. |

The most direct observed usability cost was discovery and input preparation. Participants could read sources and record Markdown without using the canonical path. The pilot does not identify a defensible type merger. The sealed evaluation should distinguish successful canonical use from the effect of tool availability. [Record use and burden](../a-gate.md#burden-and-use) · [Worked-case proof](../milestone-a.md)
