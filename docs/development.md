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

The neutrality check reads the actual CLI help, published schemas, and
`methods/general.toml` or files under `methods/general/`. Once `init` appears
in command help, the check invokes it in a temporary workspace and scans the
files it creates. Register any new default surface in the same change that
introduces it. Update the allowed crate edges and reviewed deny-list alongside
any boundary change.

Keep crash-injection property tests in the normal suite when W02.15 lands.
Add a separate LazyFS job for W02.16 and include the MCP tools-list size check
with the transport tests in W13.02.
