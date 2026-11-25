# Development

Rust 1.100.0, Clippy, and rustfmt are selected by `rust-toolchain.toml`. The
workspace MSRV (`rust-version` in `Cargo.toml`) is 1.100, and CI checks the
workspace with that toolchain too. Install Just 1.57.0 and cargo-deny 0.20.2
to match CI. If cargo-deny comes from a mise shim, the justfile selects 0.20.2
through `MISE_CARGO_DENY_VERSION`. The checks also use Python 3 from the system. The versions and
the tested configurations are listed in the
[support matrix](support-matrix.md#versions).

Run `just check` for formatting, linting, tests, crate direction, domain
neutrality, and the RustSec, license, and dependency source checks. Domain
neutrality is two checks: `scripts/check-boundaries.py` (the reviewed
deny-list) and `scripts/check-neutrality.py`, the Milestone B proof's
neutrality scan run alone, which adds named tools and security terms and
reports each allowed match with the plan text that allows it. The audit
fetches the current RustSec database. For a cached check, run
`CARGO_NET_OFFLINE=true just check-offline` after fetching dependencies and
running the audit once.

`just test` uses [cargo-nextest](https://nexte.st) when it is installed, which
runs every test binary in parallel, and runs the doctests with `cargo test
--doc` after it. Without nextest it runs `cargo test`, as CI does. The crash
and recovery tests spend nearly all their time in `fsync`, so a run with
`TMPDIR` on a tmpfs (for example `TMPDIR=/dev/shm/inv-tests just check`) is
much faster. Their faults are injected through the store's filesystem seam,
so a tmpfs does not weaken what they check. Real power-loss behaviour is the
LazyFS job's concern (`scripts/qualify-lazyfs.sh`).

The Linux offline job fetches locked dependencies first, then runs the full
Rust suite with an isolated network namespace, a clean environment, and a
PATH containing the pinned toolchain and the native linker tools. Tests should
use checked-in fixtures and temporary workspaces.

`fixtures/vault-goldens/` holds the Scenario A, B, C and D workspaces and the
vaults rendered from them. The vault tests read them and never write them. A
change to generated vault bytes needs a `RENDERER_VERSION` bump and
`python3 scripts/vault-goldens.py target/debug/investigation --write`;
`--rebuild-workspaces` reruns the scenario proofs and replaces the workspaces
too (`--only scenario-b` limits either flag to one scenario).

`investigation demo` replays these workspaces' journals: `build.rs` in
`crates/investigation-cli` embeds them, and `src/demo.rs` submits each
committed transaction again through the store under the deterministic clock
and ID seam of `investigation_core::replay_seams`. A rebuilt workspace
therefore changes the demo too; `tests/demo.rs` checks that every scenario
still replays, reproduces its bytes and runs its tutorial path, including
plan §17.5 as written.

`docs/walkthroughs/` holds one method walkthrough per scenario.
`tests/walkthroughs.rs` runs each from a fresh demo workspace through the
tutorial runner in `tests/support/tutorial.rs`, the one `tests/demo.rs` uses
for §17.5. The runner checks exit statuses, quoted output and that refusals
and dry runs commit nothing (the runner's module comment gives the format).
The walkthroughs quote the demo's real IDs, so a rebuilt scenario workspace
or a changed text output needs the walkthroughs updated as well; the test
failure names the line and prints the new output.

When the change to the output is intended, bless the quotes instead of
editing them by hand, then review the result as a diff:

```sh
INVESTIGATION_BLESS_WALKTHROUGHS=1 cargo test -p investigation-cli --test walkthroughs
git diff docs/walkthroughs/
```

Bless rewrites only the quoted `text` blocks that no longer match, and keeps
each one's shape. Lines that still match stay as written. Each changed line
is replaced by the printed line it aligns with, keeping its `$NAME`s, its
` ...` cut and the `...` lines around it. When a changed word is an ID or a
path, such as an export folder's hash, bless also replaces it in the rest of
the document, so a later `cat` of that folder follows. Exit statuses,
refusals and dry runs are still checked. A quote whose lines cannot be paired
one to one with the new output, for example because lines were inserted
inside it or it fits two places equally well, fails with its location
instead. Edit that one by hand. Bless does not change commands or
here-documents except to follow such a renamed word, so it does not repair a
walkthrough after a rebuilt scenario workspace changes the IDs the commands
use.

`docs/agent/` holds the pages that `investigation docs` prints: the agent
instructions (`agents`) and the optional skill (`skill`). They are compiled
into the binary, so they work offline. To add a page, add one line to `PAGES`
in `crates/investigation-cli/src/docs.rs`. The unit tests there parse every
command line in the pages and `README.md` with the live command tree, and
they check every operation name against the catalog. A page whose text
changes needs a new version marker and a new row in `RELEASED`. Then
`onboard` reports the new version.

Every other document is checked against the binary too (REL.02):

- `every_document_names_only_what_the_binary_has` in
  `crates/investigation-cli/src/docs.rs` reads `README.md`, `AGENTS.md`,
  everything under `docs/` and `methods/`, and the READMEs of `examples/`,
  `obsidian/`, `schemas/` and `scripts/`. Each `investigation ...` command in
  a code span or code block must parse against the live command tree, with
  its subcommands, flags and literal values, and each name shaped like an
  operation (`family.verb`, both halves used by the catalog) must be a
  catalog operation. Schematic text passes: `<ID>` and `ID` placeholders,
  `[optional]` parts, `...`, and required arguments left out. Anything else
  the binary lacks needs a row in `ALLOWED` there with its reason, such as an
  audit quoting a wrong command whose error it checked.
- `every_json_example_in_the_documents_validates_against_its_schema` in
  `tests/docs.rs` validates each fenced `json` block in those documents
  against the schema `investigation schema NAME` prints. Its `EXAMPLES` table
  names the schema of every block, or the reason a block has none; a new
  block fails until it has a row. Only the placeholders in `PLACEHOLDERS`
  (a handle or digest the binary would supply) are filled in first.
- `the_documented_versions_are_this_builds` in `tests/docs.rs` compares the
  versions in [support-matrix.md](support-matrix.md#versions) with the
  package, `rust-toolchain.toml`, `Cargo.lock`, the CI workflow and the
  contract versions the executable prints.
- `scripts/check-readme-quickstart.py` runs the README quickstart and
  [quickstart.md](quickstart.md) as written, each from an empty directory.
  CI passes `--install DIR`, which first runs the install block of
  quickstart.md from the repository root and then uses the executable it
  installed.

Generated material stays current by construction or by a check: the
`docs/agent/` pages and `methods/general.toml` are compiled in, and the
`agents-block` that `integration show` prints is built from the compiled
`agents` page; the walkthroughs run in `tests/walkthroughs.rs`; the published
`schemas/` are compared with the executable by `scripts/check-v1-schemas.py`;
the producer onboarding page is checked against the Python example by
`tests/docs.rs`.

`investigation integration apply` writes the same pages into a file the user
names, and only then: the `agents` page as a managed block between
`<!-- investigation:begin VERSION -->` and `<!-- investigation:end VERSION -->`
lines, or the `skill` page as an owned file. Remove takes out exactly the
bytes apply added. `crates/investigation-store/src/owned_file.rs` does the
file access: no symbolic links, a temporary file renamed into place only if
the target is unchanged, and the `integration.file_bytes` limit. A new
version of a page reaches installed copies on the next explicit apply.

`just bench` builds release binaries and runs the plan §10.5 performance
harness (`crates/investigation-bench`). It generates a 10,000-object /
50,000-relation workspace, the sparse 1,000 × 200 choice fixture and an
exhaustive small choice fixture through the store API, checks that grouped
`next` output is complete against the generator's ground truth, and times
warm `brief`, `next`, frontier pages, `retention preview` of one Artifact, a
single-operation `record`, `repair index` and `vault render` against the
targets. The large fixture's profile also times the retention holder scan in
process (`holder_scan`: `record_holders` and `Store::retention_obligations`),
the work `retention preview` and `retention reclaim` do under the
coordination lock. It writes a JSON report to
`target/bench/report.json` with the machine, build profile, load average,
sample counts, warmup, trial order and nearest-rank percentiles with their
n. `just check` runs only a smoke run on tiny fixtures; timings on a shared
or loaded machine are noisy, so compare runs at similar load.

`just package` builds the Linux x86_64 release candidate twice, from the committed tree at two paths, and checks that the bytes match. `just smoke-package` installs it into isolated prefixes and smoke-tests it in a bubblewrap clean root. Both are manual release steps; see [packaging](packaging.md).

`just acceptance` runs the deterministic acceptance measures of plan §20.5
(`scripts/acceptance-measures.py`): correct case targeting, lost and duplicate
writes, false promotion in invalid-control and stale-basis fixtures,
repairability, context efficiency, blindspot calibration and domain neutrality.
It runs the tests behind each measure, most of them earlier tests and the rest
in `crates/investigation-cli/tests/acceptance_measures.rs`, with
`INV_ACCEPTANCE_OBSERVATIONS` set so they record what they measured, and writes
`target/acceptance/deterministic-report.json`: per measure the target, the
observed value, pass or fail and every test with its outcome. The report labels
itself as deterministic checks; agent-trial results come from
`scripts/release-trial.py`. Its tests also run in `just check`; the CI offline
job runs the script itself without a network. It takes about 40 seconds after a
build (`--quick` skips the slow trial-task rehearsals and takes under 10).

`just load-test DIR` runs the plan §12.5 vault load test (about 50,000 notes, with and without the viewer features, hours long, not part of `just check`). An ignored test, `cargo test -p investigation-cli --test vault_load_smoke -- --ignored`, checks the harness on a tiny workspace. See [vault-load.md](vault-load.md) for the method, observations and limits.

The neutrality check reads the actual CLI help, published schemas, the
`docs/agent/` pages, and `methods/general.toml` or files under
`methods/general/`. Once `init` appears
in command help, the check invokes it in a temporary workspace and scans the
files it creates. Register any new default surface in the same change that
introduces it. Update the allowed crate edges and reviewed deny-list alongside
any boundary change.

Keep crash-injection property tests in the normal suite when W02.15 lands.
The LazyFS job for W02.16 is `.github/workflows/lazyfs.yml`. Include the MCP
tools-list size check with the transport tests in W13.02; `crates/investigation-mcp`
holds only a README until then.
