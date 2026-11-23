# Obsidian

Read-only snapshot rendering and native Bases and Canvas fixtures.

The renderer itself is the `investigation-render` crate in
`crates/investigation-render/`. Its crate documentation describes the
architecture: a pure `RenderPlan` built from one verified snapshot, an
ownership-aware publisher, and a renderer lock separate from the commit lock.
This directory holds vault fixtures; rendering never requires Obsidian to be
installed or running.

## Compatibility profile

`compatibility-profile.json` pins the supported Obsidian release (1.15.1,
verified against documentation only; the W12.07 integration gate records the
release it actually runs against), records the documentation pages and
revisions consulted, and lists every viewer feature with a tier: `baseline`
(plain Markdown), `optional` (Bases and Canvas, allowed only when the installed
release is inside the supported range) or `unsupported` (never emitted, such as
Bases row sort keys, which the syntax page does not document). An absent,
unparseable or out-of-range release resolves to the baseline.

The profile is read by `investigation_render::compat`. Its `detect` function
reads vault bytes and names the features in use, so tests check offline that
every feature a rendered vault uses is listed as emitted. A renderer change
that adds a feature must add it to the profile first. Re-check the documentation
and raise `supported_range.max` before relying on a newer release.
