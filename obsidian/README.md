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

## Native Bases

The renderer can plan one native Base beside each static table view, at
`Cases/<case-id>/Views/<view>.base`. The comparison matrix is the exception: it
stays a static table in v1 (plan §12.5). Bases are optional, and they are off
by default. To turn them on, state the viewer release in the workspace's
`investigation.toml`:

```toml
[vault_policy]
enabled = true
viewer_release = "1.15.1"
```

The working vault plans Bases only when `compat::resolve` allows every Base
feature for that release (`bases::FEATURES`). An absent, unparseable or
out-of-range release gets no `.base` file. Plan §12.3 says "Tables are the
baseline", and it asks for "feature detection or a tested compatibility
profile, not assumptions that every user has every current view type". The
renderer never contacts the viewer, so the stated release is the detection.
Without one, the vault holds no file that only some releases can read. Bases
are never emitted as inert extra files. The static tables hold the same rows,
so a vault without Bases is complete.

Every Base uses only documented syntax: a top-level `filters` with `and`/`or`,
`properties` with `displayName` (column labels only), and one `table` view
with `order`. It never uses a row sort key, formulas, summaries or another view
type. Strings are emitted by the tested YAML escaper. The filter always starts
with `inv_case_id == "<case-id>"`, so a note of another case never appears,
whatever folder it is in. Then it adds the view's record kinds:

| Base | Notes listed (besides the exact case ID) |
|---|---|
| Questions, Hypotheses, Options, Criteria, Sources, Conclusions, Decisions | `inv_kind` of that record kind |
| Evidence | artifacts, and observations whose `inv_value_kind` is not `measurement` |
| Measurements | observations whose `inv_value_kind` is `measurement` |
| Inquiries and Runs | `inv_kind` `inquiry` or `run` |
| Reviews and Blindspots, Retrospectives and Lessons | `inv_kind` `review` or `retrospective` |
| Epistemic Frontier | notes other than views with `inv_open_gaps > 0`. These rows match the static note's "Records named by an open gap" table. |
| Timeline | every note of the case except views. These rows match the static note's "Latest revision of each record" table. |

Every record note and Overview carries `inv_open_gaps`, and every observation
note carries `inv_value_kind`, even when its content is withheld. So the
Bases select the same notes as the static tables. Each property that a Base
compares with `!=` or `>` is present on every note that the Base can reach.

Base files are tool-owned and listed in the ownership manifest. Each one
depends only on its case ID and view, so identical renders and later snapshots
produce identical bytes. Editing a Base file is drift. Editing a cell in a
Base changes the underlying note's properties, which is also drift: the note
is left in place, no longer updated, and the edit is not recorded. Each view
note that has a Base says this under `## Native Base`. When Bases are turned
off, the next render removes the Base files that were not edited.

The checks run offline. `vault_fixtures` evaluates each golden Base filter
against the golden notes' frontmatter. It confirms that the matching notes are
exactly the rows of the static table, all in one case. Scenarios A and C state
release 1.15.1. Scenario D states no release, so it shows the default.
Opening the Bases in the viewer is the job of the W12.07 integration gate.
