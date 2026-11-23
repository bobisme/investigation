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
  `max_items`, or `object` with `fields` and optional `required`).
- `[[observation_guidance]]`: `name`, `title`, `text`, optional `modes`.
- `[[policies]]`: `name`, `input` (`required_fields`, `blindspot_review`,
  `closure_retrospective` or `distinct_actor`), `gate` (one of `case.close`,
  `conclusion.request_review`, `conclusion.validate`, `decision.propose`,
  `decision.accept`), `fields` for `required_fields`, and `rationale`.
- `[[views]]`: `name`, `title`, `view` (a renderer view identifier), optional
  `record_kinds`.
- `[[reports]]`: `name`, `title`, `template` (a renderer template identifier).

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
