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
