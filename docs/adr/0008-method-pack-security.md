# ADR 0008 – Method pack security and policy inputs

Accepted with pack policy inputs (W09.07); 2026-11-22. Refines the method pack line of [ADR 0004](0004-product-boundaries.md). [Pack format](../../methods/README.md)

**Packs are inert data.** A pack is a bounded TOML file decoded under the strict input rules. Keys that name hooks, commands, includes or remote references are rejected before decoding. Triggers name built-in gap rules, enumerated case facts or record kinds; a pack cannot define a predicate, run a model, scanner or command, contact a service or grant authority. The loader is a pure function of the bytes.

**Only pinned bytes count.** A pack supplies guidance or policy to a case only after an attributed `method_pack.adopt` pins its retained bytes by digest. Resolution reads the retained blob for that digest, never the ingest path, so editing a file on disk changes nothing. Moving to another revision is a new adoption that names the replaced digest. The built-in `general` pack supplies guidance but no policy, so the core has no default policy.

**Policy only tightens promotion.** Policy inputs form a closed set, each bound to named gates: `conclusion.request_review`, `conclusion.validate`, `decision.propose`, `decision.accept` and `case.close`. An input can add a requirement there (fields, a current blindspot review, a distinct actor, acknowledgment of every open objection, or extra closure triggers). It cannot relax a built-in gate or reach recording: recording, ingest and capture handlers do not consult pack policy, and a test enforces that.

**Policy is recorded, not recomputed.** Each gate stores `gate_policy` on the revision it writes: the pinned packs it consulted by ID, version, digest and availability, and the rules it applied with the records that satisfied them. An adoption applies from its sequence onward and never re-gates or rewrites earlier records. A pinned pack whose bytes are unavailable supplies no policy, but the gate records it as unavailable and the receipt warns, so losing a policy is never silent.

**Rejected alternatives.** A policy expression language was rejected because it would be executable. Failing every gate closed when a pinned pack is unavailable was rejected because retained-bytes loss should be repaired by restore or re-adoption, and the record of the gate shows what was missing. A pack-supplied default for all cases was rejected because the core must stay domain-neutral.

**Deferred.** The pack file schema is not yet published under `schemas/v1`; it will publish open vocabularies (gap rules, case facts) as validated patterns so that new built-in rules are not breaking. Workspace-level policy, distinct contexts and human confirmation for sensitive writes are W10.19.
