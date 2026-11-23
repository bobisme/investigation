# Methods

Declarative general and opt-in method packs.

A method pack is a TOML file in the `inv.method-pack.v1` format. The loader in
`investigation-core` (`method_pack`) treats it purely as data: it rejects
unknown keys, keys that look like hooks, commands, includes or remote
references, floats and dates, template or script markup in text, oversize
text, and references outside the closed vocabulary. Each rejection carries the
JSON Pointer of the failing field. Loading returns an immutable pack and the
SHA-256 digest of the file bytes; it performs no process spawn, network access
or write.

`general.toml` is the built-in pack and is compiled into the binary.

## Shape

```toml
schema_version = "inv.method-pack.v1"
id = "example"                  # lowercase kebab-case; also the extension namespace
version = "1.0.0"               # MAJOR.MINOR.PATCH
title = "One line"
summary = "One line"
modes = ["explain", "measure"]  # case modes the pack applies to

[[cards]]
name = "frame"
title = "Frame the question"
stage = "frame"                 # a workflow stage of the operation catalog
summary = "The question is ambiguous."
modes = ["explain"]             # optional subset of the pack modes
operations = ["question.create"] # active catalog operations only
triggers = [
  { case_fact = "no_question" },
  { gap_rule = "hypothesis.no_assessment" },
  { record_kind = "option" },
]

[cards.body]                    # plain prose, at most one screen
when = "..."
record = "..."
mistake = "..."                 # optional
skip = "..."
```

Triggers reference only built-in gap-rule IDs, the enumerated case facts
(`no_question`, `single_open_hypothesis`, `competing_hypotheses`,
`multiple_options`, `multiple_sources`, `measured_observation`, `stalled`) or a
record kind; a pack cannot define a predicate.

Optional sections:

- `[[properties]]`: `name`, `record_kinds`, `description` and a closed `schema`
  (`type` = `string` with `max_length` and optional `enum`, `integer` with
  optional `minimum`/`maximum`, `decimal`, `boolean`, `array` with `items` and
  `max_items`, or `object` with `fields` and optional `required`). A case
  that adopted the pack records them with `extensions` on a transaction
  operation; see [Extension properties](#extension-properties).
- `[[observation_guidance]]`: `name`, `title`, `text`, optional `modes`.
- `[[policies]]`: `name`, `input`, `gate`, `fields` for `required_fields`,
  optional `triggers` for `closure_retrospective`, and `rationale`. See
  [Policy inputs](#policy-inputs).
- `[[guardrails]]`: `name`, `operation`, `requirement` and `rationale`. See
  [Write-side guardrails](#write-side-guardrails).
- `[[views]]`: `name`, `title`, `view` (a renderer view identifier), optional
  `record_kinds`.
- `[[reports]]`: `name`, `title`, `template` (a renderer template identifier).

## Policy inputs

A pinned pack can add requirements at promotion and closure gates. It cannot
relax a built-in gate, and no input applies to recording: `observation.add`,
`source add`, `ingest` and `capture` are never refused for pack policy. Each
input is bound to the gates it can tighten; a pack that declares another
combination is rejected when it loads.

| `input` | Gates | Adds |
|---|---|---|
| `required_fields` | `conclusion.request_review`, `conclusion.validate`, `decision.propose`, `decision.accept`, `case.close` | Each dotted name in `fields` must be present on the gated record (the Conclusion, Decision or Case) and not null, blank, empty or an explicit `unknown`. Names under `extensions` also resolve in the record header's extension map. |
| `blindspot_review` | `conclusion.validate`, `decision.accept` | A current `review.submit` of `kind: blindspot` on the subject: submitted, about a revision with the same substantive basis, and with every read-set revision still current. |
| `distinct_actor` | `conclusion.validate`, `decision.accept` | The promoting actor did not record the subject's current substantive content. Actors are attributed claims, not authenticated identities. |
| `isolated_review` | `conclusion.validate`, `decision.accept` | A current review of the subject (validation or blindspot) with no low-independence flag: its declared `context_isolation` is not `same_session_self_review`, it does not declare `work_kind: self_review`, and its recording actor did not record the subject's current content. Declarations are attributed claims; the rule never certifies cognitive independence. |
| `acknowledge_open_objections` | `decision.accept` | Every open objection on the decision or its basis needs an acknowledgment, not only critical ones. |
| `closure_retrospective` | `case.close` | The case is material, so closing needs a current Retrospective instead of a waiver, when any of `triggers` holds (a `case_fact` or `record_kind`; no `gap_rule`). Without `triggers`, every closure is material. |

```toml
[[policies]]
name = "challenge-before-accept"
input = "blindspot_review"
gate = "decision.accept"
rationale = "Consequential choices get a recorded challenge first."
```

An unmet rule fails the gate with `GATE_FAILED` (`RETROSPECTIVE_REQUIRED` at
closure) and `error.expected.policy_blockers`: one entry per rule with the pack
ID, version and digest, the policy name, input, gate, rationale, what is unmet
and, where one exists, `repair_schema`.

Only available pinned packs supply policy; the built-in pack supplies none.
Every gate stores `gate_policy` on the revision it writes (on a Case, under
`closure`): the gate, `rule_version`, the acceptance `sequence`, every pinned
pack it consulted with its status, and the rules it applied with the records
that satisfied them. A pin whose bytes are unavailable supplies no policy; the
gate records it as unavailable and the receipt warns, so the change is never
silent. Policy applies from adoption onward: earlier accepted records are not
re-gated when a pack is adopted or replaced.

## Write-side guardrails

A pinned pack can also separate promotion and suppression from the context
that read hostile evidence. A guardrail adds one requirement to one of five
operations: `conclusion.validate`, `decision.accept`, `source.withdraw`,
`observation.retract` or `classification.downgrade`. No other operation can
be guarded. Guardrails are opt-in: without one, nothing changes.

| `requirement` | Adds |
|---|---|
| `distinct_actor` | The writing actor did not record the subject's current content. |
| `distinct_context` | The command declares a `session` that recorded none of the subject's current content and has no recorded `evidence read` of untrusted material under that label. |
| `human_confirmation` | The envelope carries `human_confirmation`: `confirmed_by`, `method` (how it was obtained) and `confirms`, which names this operation and the subject's ID. |

```toml
[[guardrails]]
name = "separate-validator"
operation = "conclusion.validate"
requirement = "distinct_context"
rationale = "The context that read raw evidence does not validate what it supports."
```

An unmet requirement fails with `GATE_FAILED`, writes nothing and lists
`error.expected.guardrail_blockers`, each with a `repair`. A satisfied write
stores the guardrails it met with the revision it writes. Actors, sessions and
confirmations are self-reported claims: `investigation` does not authenticate
people or contexts, and sees only evidence reads made through it.

## Adoption per case

Loading a pack does not adopt it. The built-in `general` pack always applies
and cannot be pinned. Any other pack applies to a case only after an explicit,
attributed adoption:

1. Retain the pack file through the standard ingest path (`ingest` or
   `source add`). The case then holds the exact bytes as an Artifact.
2. Submit `method_pack.adopt` with the exact Artifact revision, the file
   digest, the `id` and `version` declared in the file and a reason. Include
   the current case revision in the transaction preconditions. The store loads
   the retained bytes under the commit lock and rejects the adoption unless
   they load as the declared pack.

The pin (ID, version, digest, Artifact revision, reason and acceptance
sequence) is stored on a new Case revision; the journal event carries the
actor. To move to another revision of a pinned pack, retain the new file and
adopt it with `replaces` set to the digest that is pinned now. Earlier Case
revisions keep the pins they had, so work recorded before the change still
resolves the policy it was made under.

Resolution reads only the retained bytes for the pinned digest, never the
original path, so editing the file on disk changes nothing for the case. A pin
whose bytes are missing, altered or no longer loadable is reported as
unavailable (`show CASE_ID --depth full` lists `method_packs`); recording,
reading and export continue.

## Extension properties

A declared property is an optional field on an existing record kind, so a
pack can add fields without a change to any Rust record type. A transaction
operation carries them beside its payload, keyed by the pack ID:

```json
{"op": "observation.add", "payload": {"...": "..."},
 "extensions": {"bench-notes": {"warmup_runs": 5, "machine": {"cpu": "x86-64"}}}}
```

The store checks them when it commits the transaction, against the schema
of the pinned pack revision that it reads from the retained bytes. The values
are stored on the header of the record that the operation creates, or of the
single record that it revises, under `extensions.<pack id>`. A refused value
names its JSON Pointer. A namespace the case has not adopted is refused with
a `method_pack.adopt` repair. The core never reads the values for a gate,
freshness, ranking, blindspot check or basis digest. Only the pack's own
`required_fields` policy can name them. Reads label them as pack data, and
replay does not need the pack file.
[`fixtures/method-packs/bench-notes.toml`](../fixtures/method-packs/bench-notes.toml)
is a complete example with a card and four properties.
