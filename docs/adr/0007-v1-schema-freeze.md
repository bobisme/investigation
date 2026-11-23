# ADR 0007 – v1 schema freeze

Accepted after the directional pilot disposition and sealed release preregistration. The preregistration was integrated at `214c45b`. The catalog is published under `schemas/v1/` from the same declarations that the CLI uses for validation and discovery. [Pilot disposition](0006-a-gate-catalog-disposition.md) · [Sealed preregistration](../release-trial.md)

Publish `inv.command.v1`, `inv.response.v1` and `inv.ingest.v1`, including active payload schemas and the deferred name registry. Preserve alpha request tags for replay and exact idempotent retries. Retain opaque source metadata in the declared optional object. Storage codecs have independent compatibility requirements and keep recorded evidence and snapshot identities exact.

CI compares the generated files and checks the previous v1 contract for compatibility. Additive optional fields are allowed when they preserve existing valid requests, and incompatible changes require a new version. A replay check qualifies the original alpha pilot journals under the v1 binary. [Consumer contract](../schema-v1.md) · [Published schemas](../../schemas/README.md)
