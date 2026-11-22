# Scenario A: intermittent request timeout

All bytes here are synthetic and offline. The baseline pairs the same named
build, workload, route, and data set across a slow request and a fast control.
`baseline/manifest.json` records the role and SHA-256 digest of every source.
`expected.json` describes properties for a successor, not generated object IDs
or a required narrative.

## By-role reading guides

- `roles/investigator.md` lists a source-first reading order and limits claims to
  the captured build and workload.
- `roles/reviewer.md` highlights the clock-confounded trace and different-build
  trap that must remain visible as gaps.

## Variants

- `broken-clock/` is independently replayable and keeps its unaligned trace as
  a source, but its wall clock cannot order the events against pool telemetry.
- `different-build-trap/` is independently replayable and contains the only
  apparent scheduler-only refutation, recorded from another build.

Each variant has its own manifest, digest list and expected properties. Source
files are immutable seed bytes; changes should create a new variant or revision.
