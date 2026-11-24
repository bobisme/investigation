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
- A render writes about 25 to 30 ms per note, and 99% of that is `fsync` (about two per note). PERF.07 replaced the per-file syncs with batches; see below.
- At the measured commit, brief, next and status on one small case cost as much as on the whole workspace, because they verified every case. Since PERF.06 (bn-24bx, ADR 0011), `brief`, `next`, `show` and `record` verify only the cases they read: at 650 cases they take 15, 19, 13 and 85 ms. `vault status` still reads the whole workspace.

Against the plan's §10.5 targets (warm brief/next under 250 ms p95 and mutation under 150 ms p95, set for 10,000 objects in one case): at 60 cases (3,840 objects) brief and next met them and `record` was at the limit; from about 8,000 objects over 130 cases they were missed by 4x to 10x and at 41,600 objects over 650 cases by 2x to 4x on a less loaded moment. Slow commands are filed as PERF bones under bn-9muo: PERF.06 (bn-24bx, commit and read cost grows with the number of cases), PERF.07 (bn-kxx5, render fsync and unchanged-render cost), PERF.08 (bn-171a, `repair index`).

## After PERF.07 (bn-kxx5)

The publisher now makes vault files durable in batches of 2,048, with one filesystem barrier per batch, and an unchanged render skips copies of the plan, needless manifest stores, and reading files whose recorded stat fingerprint still matches ([ADR 0012](adr/0012-batched-vault-durability.md)). Same machine and load as above, `just load-test` at 130 cases, baseline variant, before (main at 1a4c6cb) and after:

| 130 cases, 9,881 notes | Before | After |
| --- | --- | --- |
| Full render | 283 s, 258 MiB | 1.3 s, 196 MiB |
| Render, nothing changed (n=3) | 1.51 s, 267 MiB | 1.00 s, 201 MiB |
| Render, one record changed (n=3) | 1.73 s | 1.09 s |
| First render after the drift edits | 6.5 s, 334 MiB | 1.65 s (measured by hand, n=1) |

At 650 cases (49,401 notes), on two copies of one generated workspace with alternating order: full render 1,369 s, 1.20 GiB before and 9.4 s, 0.90 GiB after; the first render after it 24.2 s before (the 23.5 s above) and 9.7 s after; later unchanged renders 6.3 to 8.3 s, 1.28 GiB before and 5.2 to 5.3 s, 0.95 GiB after. The vaults are byte-identical. What remains of an unchanged render at this size is mostly planning (3.6 s) and reading the snapshot (0.7 s). After PERF.06 (case-scoped verification) the unchanged render measured 5.6 to 6.2 s before and 5.1 to 5.4 s after at 650 cases, and 1.16 to 1.24 s before and 0.98 to 1.05 s after at 130 cases.

## After PERF.10 (bn-2c2m)

Cases are planned in parallel, and the snapshot read keeps each case's state but drops its events as soon as it is verified ([ADR 0012](adr/0012-batched-vault-durability.md#perf10-planning-in-parallel-a-streamed-snapshot-read-and-what-power-loss-was-tested-against-bn-2c2m-2026-11-24)). At 650 cases (49,406 notes), A/B alternating on one rendered copy at load average 52 to 66: the unchanged render went from 5.15 to 5.75 s and 0.88 to 0.96 GiB to 1.88 to 2.13 s and 0.72 to 0.74 GiB (each side had one outlier under load: 11.4 s before, 5.25 s after), and the full render from 7.0 s and 0.92 GiB to 3.5 s and 0.68 GiB, with byte-identical vaults. `vault_render_unchanged` now carries a budget in the load report: 0.06 ms per planned note (at least 250 ms) and 16 KiB of peak memory per planned note plus 32 MiB.

## Not measured

The acceptance for this bone also names Obsidian's own behaviour: cold open, a Base query for one case, Canvas open and search. The harness runs headless and does not drive Obsidian. Those figures are not in this page and must not be inferred from it. When someone measures them, record the Obsidian release, the machine, the vault (use `--fixtures` to keep the generated one), and counts, repetitions and order, as the harness does.

`scripts/obsidian-ui-check.py timings` (W12.07) measures them through the viewer's CLI, outside `just check`, against a kept vault: `TMPDIR=/var/tmp just load-test ~/vault-load-10k --cases 130 --samples 1` (under your home folder, so a sandboxed viewer can open it), open `~/vault-load-10k/viewer_1_15_1/vault` in the viewer once, then `just obsidian-ui timings --vault ~/vault-load-10k/viewer_1_15_1/vault --samples 5` (add `--allow-restart` for cold opens; it restarts the whole app). It writes `target/obsidian-ui/timings.json` and a table in this page's form, with the release, machine, counts, samples and order. No such run is recorded yet (see obsidian/README.md, "Integration gate").

## Guidance

- Tested scale: up to 49,401 generated notes in one vault and 650 cases. Below about 5,000 notes every measured command finished within a few seconds on this machine, apart from the first render. Treat 10,000 notes as the point where a full render takes minutes and commits and reads take about a second.
- A full render takes about 0.2 s per 1,000 notes since PERF.07 (it was about half a minute). It happens once, and again only when the renderer version changes or the vault is deleted. After that, an unchanged render reads only the notes whose stat fingerprint changed, but it still plans the whole vault, on up to 8 threads; at 50,000 notes it takes about 2 s.
- Per-case Bases already filter by exact case ID, so a Base reads only its own case's properties when a case view is opened. Obsidian may still index every note in the vault.
- Mitigations that change no canonical state: keep one vault per active case or per review (`vault render` renders the whole workspace today; use a separate workspace archive or a review export for a closed case), archive closed cases out of the working vault by moving them to a different workspace, and keep the viewer features off for a very large vault, since they add about 16% more files. Deleting or moving generated notes is drift: it is detected and reported, never written back to the journal.
- Do not edit generated notes while measuring; edits are drift and are kept as found.
