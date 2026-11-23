# Development

Rust 1.98.1, Clippy, and rustfmt are selected by `rust-toolchain.toml`. The
workspace MSRV is 1.89, the first stable release with `std::fs::File::lock`.
Install Just 1.57.0 and cargo-deny 0.20.2 to match CI. The checks also use
Python 3 from the system.

Run `just check` for formatting, linting, tests, crate direction, domain
neutrality, and the RustSec, license, and dependency source checks. The audit
fetches the current RustSec database. For a cached check, run
`CARGO_NET_OFFLINE=true just check-offline` after fetching dependencies and
running the audit once.

The Linux offline job fetches locked dependencies first, then runs the full
Rust suite with an isolated network namespace, a clean environment, and a
PATH containing the pinned toolchain and the native linker tools. Tests should
use checked-in fixtures and temporary workspaces.

`fixtures/vault-goldens/` holds the Scenario A, C and D workspaces and the
vaults rendered from them. The vault tests read them and never write them. A
change to generated vault bytes needs a `RENDERER_VERSION` bump and
`python3 scripts/vault-goldens.py target/debug/investigation --write`;
`--rebuild-workspaces` reruns the scenario proofs and replaces the workspaces
too.

`docs/agent/` holds the pages that `investigation docs` prints: the agent
instructions (`agents`) and the optional skill (`skill`). They are compiled
into the binary, so they work offline. To add a page, add one line to `PAGES`
in `crates/investigation-cli/src/docs.rs`. The unit tests there parse every
command line in the pages and `README.md` with the live command tree, and
they check every operation name against the catalog. A page whose text
changes needs a new version marker and a new row in `RELEASED`. Then
`onboard` reports the new version.

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
warm `brief`, `next`, frontier pages, a single-operation `record`, `repair
index` and `vault render` against the targets. It writes a JSON report to
`target/bench/report.json` with the machine, build profile, load average,
sample counts, warmup, trial order and nearest-rank percentiles with their
n. `just check` runs only a smoke run on tiny fixtures; timings on a shared
or loaded machine are noisy, so compare runs at similar load.

`just load-test DIR` runs the plan §12.5 vault load test (about 50,000 notes, with and without the viewer features, hours long, not part of `just check`). An ignored test, `cargo test -p investigation-cli --test vault_load_smoke -- --ignored`, checks the harness on a tiny workspace. See [vault-load.md](vault-load.md) for the method, observations and limits.

The neutrality check reads the actual CLI help, published schemas, the
`docs/agent/` pages, and `methods/general.toml` or files under
`methods/general/`. Once `init` appears
in command help, the check invokes it in a temporary workspace and scans the
files it creates. Register any new default surface in the same change that
introduces it. Update the allowed crate edges and reviewed deny-list alongside
any boundary change.

Keep crash-injection property tests in the normal suite when W02.15 lands.
Add a separate LazyFS job for W02.16 and include the MCP tools-list size check
with the transport tests in W13.02.
