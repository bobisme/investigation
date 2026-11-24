# ADR 0007 – v1 schema freeze

Accepted after the directional pilot disposition and sealed release preregistration. The preregistration was integrated at `214c45b`. The catalog is published under `schemas/v1/` from the same declarations that the CLI uses for validation and discovery. [Pilot disposition](0006-a-gate-catalog-disposition.md) · [Sealed preregistration](../release-trial.md)

Publish `inv.command.v1`, `inv.response.v1` and `inv.ingest.v1`, including active payload schemas and the deferred name registry. Preserve alpha request tags for replay and exact idempotent retries. Retain opaque source metadata in the declared optional object. Storage codecs have independent compatibility requirements and keep recorded evidence and snapshot identities exact.

CI compares the generated files and checks the previous v1 contract for compatibility. Additive optional fields are allowed when they preserve existing valid requests, and incompatible changes require a new version. A replay check qualifies the original alpha pilot journals under the v1 binary. [Consumer contract](../schema-v1.md) · [Published schemas](../../schemas/README.md)

## Additive enum values (owner decision, 2026-11-23)

A frozen v1 enum may gain a value only where every reader of that value must already tolerate one it does not know. Values are added, never removed or renamed, and nothing else about the enum changes. Any other enum change needs a new version.

`scripts/check-v1-schemas.py` holds the open enums in `OPEN_ENUMS`, each with the reason the rule holds for it, and refuses a new value anywhere else. It also refuses an open enum whose definition appears in another published schema, so a response, receipt or error schema cannot start to carry it unnoticed. A self-test checks both directions on every run.

The first open enum is `command.schema.json` `$defs/GuardedOperation` (bn-6fpb). It constrains only `human_confirmation.confirms[].op` in a command. Its one reader is the binary that validates the request: a binary that does not know a value refuses it with `INVALID_INPUT` at that path, as for any invalid input, and every request that was valid stays valid. It gained `artifact.withdraw` and `artifact.expire`, the two Artifact writes that suppress evidence as `source.withdraw` does.

Consequence: a binary built before a value existed may fail to read a journal whose commands or records name it. The rule covers the published request contract, not reading newer workspaces with older binaries, which the workspace format marker governs.
