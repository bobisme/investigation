# Investigation v1 contracts

The public transaction envelope is `inv.command.v1`. Writes and structured errors use `inv.response.v1`. Local import manifests use `inv.ingest.v1`. The published JSON Schemas cover commands, normal and import receipts, structured errors, evidence selectors, and each active operation's payload. The catalog also lists reserved names so later implementations can claim them without collisions. [Published catalog](../schemas/v1/catalog.json)

## Using the contract

`investigation schema --list --include-deferred --format json` lists active and reserved names. `investigation schema OPERATION --schema-only --format json` emits the exact payload shape. With `--schema-only` omitted, discovery includes examples, lifecycle and repair guidance. A reserved name has no active write handler. The `capture` wire name is reserved. The synopsis role remains part of the annotation schema, and record extensions remain namespaced separately.

Put optional producer metadata in `source.metadata` on an ingest manifest. The Source record retains that object as opaque JSON and identifies the original bytes separately. Its contents carry neither additional authority nor effects. Required unsupported envelope versions produce a structured rejection before a transaction is committed.

Put a producer namespace and stable external ID in `source.acquisition_key` to name one logical acquisition. Each path or stdin name is one item of it. A later ingest of the same key and item reuses the recorded Source. If the description and bytes are unchanged, nothing is committed and the item reports `deduplicated` with a `prior` link. A changed description gives the next revision of that Source, and changed bytes give it another retained Artifact. Either one reports `revised`, never a new acquisition. An acquisition belongs to the case that first recorded it, and a reimport cannot lower its classification. Without a key, each ingest is its own acquisition. Identical bytes still share one stored blob, but acquisitions are never merged. The key is a producer claim, not verified identity. Runtime validation enforces revision preconditions, selector integrity, actor attribution and promotion rules.

## Compatibility

The v1 readers accept the alpha command and ingest tags used by the pilot. Existing alpha receipts remain readable where serialized locally. Resubmitting identical alpha bytes under the original actor and command ID returns the original transaction and snapshot. Submit new requests with v1. A retry keeps the original envelope version as part of the original request. The journal retains its versioned storage encoding, event bytes, hashes and historical record revisions. Other auxiliary read payloads carry their own version tags.

Published v1 schemas allow additive optional properties that preserve every existing valid request. Required fields, existing value constraints and published operation names are stable. Incompatible changes need a new version and a documented migration. Reserved names may become active. The CI workflow regenerates the catalog from the binary, checks published files, and checks compatibility against the push or pull-request base. [Publication check](../scripts/check-v1-schemas.py)

The v1 qualification also extracts the original pilot handoffs, verifies their journals, rebuilds the index, and resolves and resubmits the archived commands. [Alpha replay check](../scripts/check-v1-legacy.py) · [Pilot archive and provenance](../training/a-gate/replay.md)
