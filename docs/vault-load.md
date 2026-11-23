# Vault load test (plan §12.5)

A long-lived workspace has one Markdown note per record, so it can reach tens of thousands of generated files. This page states the scale that was tested, what was observed and what is not known. These are observations with their conditions, not guarantees.

## Run it

```bash
TMPDIR=/var/tmp just load-test /var/tmp/vault-load --samples 5   # about 50,000 notes; hours; several GiB
TMPDIR=/var/tmp just load-test /var/tmp/vault-load-10k --cases 130 --samples 1   # about 10,000 notes
TMPDIR=/var/tmp cargo test -p investigation-cli --test vault_load_smoke -- --ignored   # 2 cases, 30 s, checks the harness
```

The directory must be absent or empty and is left in place for you to inspect and remove. `just check` does not run any of this. The report is `target/bench/load-report.json` (`--out` changes it). Peak memory needs GNU `time` at `/usr/bin/time`.

The generator (`investigation_bench::fixtures::load`) builds the workspace through the public store API from one seed (`--seed`, default 20261123): many cases of 64 records each. 650 cases give 41,600 objects and 49,401 notes in the plain-Markdown vault. Nothing generated is checked in. The workspace is generated once and copied for two variants: `baseline` (no `viewer_release`) and `viewer_1_15_1` (`[vault_policy] viewer_release = "1.15.1"`, which adds one Base file per Bases view and one Canvas per case map). The run order is baseline first, then the viewer variant; within a variant it is: full render, unchanged renders, status, repetitions of (record, status, render), warm reads, drift (edit 90 and delete 10 generated notes, status, render, status, conflicts, `reconcile --dry-run`), index rebuild.

## Observed (commit 439ad79 plus this bone; release build)

Machine: Threadripper (64 logical CPUs, 125 GiB), ext4 on NVMe, shared with other agents' builds and benchmarks; load average 17 to 35 during every run, so absolute times are noisy and only the scaling is reliable. Page cache was warm. Every figure below is wall time of the real binary, 1 sample unless n is given.

| Cases | Objects | Notes (baseline / viewer) | Full render | Render, nothing changed | `vault status` | one `record` | `brief --case` |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 6 | 384 | 457 / 529 | 11.8 s | 0.11 s | 0.02 s | 0.12 s | 0.02 s |
| 60 | 3,840 | 4,561 / 5,281 | 112 s / 119 s | 0.57 s | 0.11 s | 0.15 s | 0.10 s |
| 130 | 8,320 | 9,881 / 11,441 | 276 s / 290 s | 10.6 s / 7.8 s | 2.6 s / 1.5 s | 1.6 s / 1.0 s | 2.1 s / 1.3 s |
| 650 | 41,600 | 49,401 / n.a. | about 25 to 30 min (estimated from file counts during the run) | 23.5 s, 1.4 GiB RSS | 0.90 s, 0.64 GiB | 0.55 s, 0.59 GiB | 1.01 s, 0.65 GiB |

The 130-case row ran under heavier machine load than the others. The 650-case baseline report was lost when the run was stopped; its read-side figures were re-measured by hand on the same rendered workspace, and no 650-case viewer run exists. Further 650-case observations: generating the workspace took about 70 minutes, because the cost of each commit grew with the number of cases (about 18 ms for the first cases, about 0.8 s for the last); `repair index` was still running after 10 minutes.

Other observations at 60 and 130 cases:

- Drift detection: `vault status` does not see hand edits made since the last render (it reads the manifest and snapshot heads). The first render after the edits detects and records them (4.2 s at 4.5k notes, 8.4 s at 9.9k), after which `status` reports every one (100 of 100), `vault conflicts` takes 20 to 250 ms and `vault reconcile --dry-run` over 90 paths takes 22 to 140 ms.
- The viewer features add 1,170 Base files (1.1 MB) and 130 Canvas files (1.5 MB) at 130 cases: 16% more files, 8% more bytes, 2 to 5% more full-render time.
- Render manifest: 5.1 MB at 9.9k notes (about 520 bytes per note), so about 25 MB at 50k.
- A render writes about 25 to 30 ms per note, and 99% of that is `fsync` (about two per note).
- Brief, next and status on one small case cost as much as on the whole workspace: they verify every case (see PERF.06, bn-24bx).

Against the plan's §10.5 targets (warm brief/next under 250 ms p95 and mutation under 150 ms p95, set for 10,000 objects in one case): at 60 cases (3,840 objects) brief and next met them and `record` was at the limit; from about 8,000 objects over 130 cases they were missed by 4x to 10x and at 41,600 objects over 650 cases by 2x to 4x on a less loaded moment. Slow commands are filed as PERF bones under bn-9muo: PERF.06 (bn-24bx, commit and read cost grows with the number of cases), PERF.07 (bn-kxx5, render fsync and unchanged-render cost), PERF.08 (bn-171a, `repair index`).

## Not measured

The acceptance for this bone also names Obsidian's own behaviour: cold open, a Base query for one case, Canvas open and search. The harness runs headless and does not drive Obsidian. Those figures are not in this page and must not be inferred from it. When someone measures them, record the Obsidian release, the machine, the vault (use `--fixtures` to keep the generated one), and counts, repetitions and order, as the harness does.

## Guidance

- Tested scale: up to 49,401 generated notes in one vault and 650 cases. Below about 5,000 notes every measured command finished within a few seconds on this machine, apart from the first render. Treat 10,000 notes as the point where a full render takes minutes and commits and reads take about a second.
- A full render is slow (about half a minute per 1,000 notes). It happens once, and again only when the renderer version changes or the vault is deleted. After that, an unchanged render reads and hashes every note (23 s at 49k notes).
- Per-case Bases already filter by exact case ID, so a Base reads only its own case's properties when a case view is opened. Obsidian may still index every note in the vault.
- Mitigations that change no canonical state: keep one vault per active case or per review (`vault render` renders the whole workspace today; use a separate workspace archive or a review export for a closed case), archive closed cases out of the working vault by moving them to a different workspace, and keep the viewer features off for a very large vault, since they add about 16% more files. Deleting or moving generated notes is drift: it is detected and reported, never written back to the journal.
- Do not edit generated notes while measuring; edits are drift and are kept as found.
