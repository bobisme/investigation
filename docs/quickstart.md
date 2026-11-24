# Quickstart

This page takes you from a checkout of this repository to a case with a retained source, a handoff brief, a ranked next step, a rendered vault and an exported report. CI runs both command blocks below exactly as written: the install from the repository root, then the rest from an empty directory. [What CI runs](../scripts/check-readme-quickstart.py)

## Install

You need the Rust toolchain that `rust-toolchain.toml` pins (see [Versions](support-matrix.md#versions)) and a local Linux filesystem from the [support matrix](support-matrix.md). From the repository root:

<!-- install:start -->
```sh
cargo install --locked --path crates/investigation-cli
```
<!-- install:end -->

This installs one executable, `investigation`, into Cargo's install directory (`~/.cargo/bin` unless `CARGO_INSTALL_ROOT` says otherwise), which must be on `PATH`. Nothing else is installed: no alias, no agent instructions file and no background process. Some people add `inv` as a shell alias; every document uses `investigation`.

## First case

Run these commands in an empty directory. Each prints one JSON object.

<!-- quickstart:start -->
```sh
investigation init casework --format json
printf '%s\n' 'The connection stayed checked out while the callback waited.' |
  investigation --workspace casework --actor agent:quickstart start 'What held the connection?' --attach - --command-id first-source --format json
investigation --workspace casework brief --level standard --format json
investigation --workspace casework next --format json
investigation --workspace casework vault render --format json
investigation --workspace casework export --kind case-report --audience internal --out report --format json
```
<!-- quickstart:end -->

What each step does:

1. `init` creates the workspace `casework/`: canonical state in `state/`, a Markdown vault in `vault/`, and `investigation.toml`. Pass `--no-vault` for a workspace without a vault.
2. `start` opens an exploratory case and retains the text from standard input as its first source in the same transaction. The receipt names the case, the source and the retained artifact with its digest. `--command-id` names the transaction, so `investigation --workspace casework --actor agent:quickstart receipt first-source --format json` recovers the same receipt after a lost response.
3. `brief` is the handoff a successor reads first, at the committed snapshot.
4. `next` ranks up to three open gaps by recorded structure. It is a heuristic, not a probability. A case that holds only a question and a source has no gap to rank yet, so `recommendations` is empty here; `frontier_argv` is the command that lists every gap at the same snapshot.
5. `vault render` writes the casebook into `casework/vault/` from the verified snapshot. Start at `casework/vault/Start Here.md`.
6. `export` writes `report/case-report.md` and `report/export-manifest.json`, which lists the snapshot, the audience and the digest of each file.

Next, record an observation with `investigation schema observation.add --format json` for the payload shape, or learn the method on a worked case with `investigation demo ./demo-case --scenario debug`. Agents read `investigation docs agents`; programs that record runs read `investigation docs producer`.
