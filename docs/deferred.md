# Deliberately deferred features

This register mirrors plan §22 (revision 4, 2026-11-21) and records the later revision-4 deferrals alongside it. REL.03 uses this register as the source for the release's public deferred-feature list. Deferred means absent from the implementation contract until its stated condition is met; it is not a hidden stub or a successful no-op. Revisit a binding decision only for an observed conflict, with a recorded ADR and corresponding tests.

| Deferred feature | Why / entry condition |
|---|---|
| Mandatory domain-specific adapters | Would violate dependency inversion; consumers integrate the neutral contract separately. |
| Core action runner or model integration | Existing tools/harnesses already act and reason; this is an evidence/decision system. |
| Full design/architecture generation | Evaluating designs is in scope; creating them is done by existing agents/other software. |
| Auto-calibrated Bayesian scores / mandatory multi-agent debate | Requires actual calibration/evidence of improved decisions; agent independence is not guaranteed. |
| Executable methodology plugin system | Start with declarative cards and schema-bound properties; code execution would enlarge trust surface. |
| General-purpose property graph database | Typed objects and SQLite relations sufficient; wait for measured need. |
| Mandatory vector search | FTS + exact provenance first; embedding optional later. |
| Autonomous blindspot agents or universal semantic gap detection | Core supports deterministic structure checks and external attributed challenge reviews; no LLM or false completeness guarantees. |
| Automatic method-pack self-improvement / cross-case doctrine promotion | Retrospectives are case-scoped; a future explicit, reviewed and sensitivity-aware pack-authoring workflow may adopt proposed lessons. |
| Distributed CRDT multiwriter or cloud case sync | Need real conflict policy, identity and consistency semantics; local multi-process first. |
| Always-on daemon or generic local REST API | CLI and optional stdio MCP suffice initially. |
| Full automatic two-way Markdown editing | Generated notes + human notes + explicit reconciliation protects canonical state. |
| Required Obsidian community plugins | Native Bases, Canvas and Markdown cover a useful baseline. |
| Formal custody or authenticity attestation | Hashes are integrity checks, not legal authentication; add verified trust anchors only for actual need. |
| Event-text physical purge workflow and remote-copy revocation | Requires separate epoch/redaction/archive semantics; don't mislabel a retraction as erasure. Entry condition: a specified and tested workflow that builds a sanitized new storage epoch or archive, keeps a redaction audit and mapping without the secret, accounts for blobs shared across cases and archive pins, and states which historical replay guarantees changed. Until then `investigation exposure` diagnoses where the text lives and `investigation docs exposure` documents rotation and the manual restore or fork remediation. Distributed copies cannot be recalled automatically. |
| Universal model of economic decision utility | Costs and preferences are context-specific; versioned criteria/assumptions suffice initially. |
| Typed `causal_role` tags on records | The `causal-structure` card teaches roles and the `failure_only_selection` check is adopted. Defer typed tags until trials show agents fill them meaningfully rather than mechanically. |
| Selector exporters (including Web Annotation interchange) | The mapping is documented; implement an exporter only when a consumer needs interoperable serialized selectors and its validation behavior can be specified. |
| Receipt exporters (including in-toto/DSSE) | Receipt semantics and standards are documented; implement export only for a concrete consumer and a defined trust/verification boundary. |
| Formal receipt signature verification, and the `externally_attested` chronology it would support | Receipts are structure checked only: the highest recorded level is `structure_checked`, and `signature_verified` and `externally_attested` are never recorded (see [Executor receipts and chronology](recording.md#executor-receipts-and-chronology)). Hashes are integrity checks, not authentication. Revisit only with a concrete trust-anchor and verification requirement, and a defined binding of the execution event's ordering. |
| PDF export | Optional later exporter; revisit when a demonstrated audience or workflow requires it and accessibility/layout can be qualified. |
| Semantic embeddings | Exact provenance and FTS are the baseline. Revisit only when a measured retrieval gap remains and embedding provenance, privacy and offline behavior are specified. |
| Large and archived captures | The alpha retains bounded snapshots. Revisit with the streaming and archive suites. |
| Automatic qualification of named statistics and sampled measurements | Require typed observation evidence for the chosen statistic and denominator. Comparisons can already record these as attributed claims. |
| Reclaim abandoned import preparation | Capture preparation runs outside the commit lock. Revisit with an explicit ownership and expiration protocol. |

The baseline tool must report unsupported/deferred operations honestly. It must not accept a command as successful when the requested feature is absent.
