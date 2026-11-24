# Method walkthroughs

Four worked investigations, one per scenario of plan §17. Each walkthrough
teaches the method, not only the commands: why each step is taken, what to
look at in the output, and what would change the conclusion. Each also shows
the scenario's adversarial variant and at least one refused write with its
repair.

| Walkthrough | Scenario | Kind of case | Adversarial variant |
|---|---|---|---|
| [A: an intermittent request timeout](a-request-timeout.md) | `debug` (§17.1) | explanatory | a refutation from a different build |
| [B: a p99 latency regression](b-latency-regression.md) | `performance` (§17.2) | measurement | "no improvement" under a broken monitor |
| [C: an architecture choice](c-architecture-choice.md) | `choice` (§17.3) | evaluative | a hard threshold changed after the decision |
| [D: contradictory source research](d-source-research.md) | `research` (§17.4) | source-based | instructions planted in a crawled page |

The same binary, operation catalog, storage and viewer handle all four; only
optional method cards change. Each walkthrough names the built-in cards that
apply (read one with `investigation guide NAME`) and the optional example pack
in [`methods/`](../../methods/README.md) that specializes them. Payloads are
described by `investigation schema OPERATION` and by the published schemas in
[`schemas/v1/`](../../schemas/v1/).

## Running a walkthrough yourself

Start in an empty directory. Each walkthrough begins with
`investigation demo ./demo-case --scenario NAME`, which builds the worked case
with real IDs. The same scenario always gives the same IDs, so you can paste
the commands as written. Then run each command in order.

The quoted outputs are excerpts:

- `...` on its own line stands for lines that are left out. A line that ends
  in ` ...` is cut short.
- `<WORKSPACE>` stands for the absolute path of `./demo-case`.
- `$NAME` stands for an ID that a write returned, such as a new Source.
  Where a quoted output shows `$NAME`, set it in your shell to the ID printed
  there (for example `NAME=src_...`) before you run the commands that use it.

A command whose comment says it is refused exits with that status and
commits nothing. A `--dry-run` checks a write as a commit would and commits
nothing.

## How they are checked

`crates/investigation-cli/tests/walkthroughs.rs` runs every walkthrough in CI
(`just check`) through the same tutorial runner that runs the plan's §17.5
tutorial path (`tests/support/tutorial.rs`, used by `tests/demo.rs`). For each
walkthrough it creates a fresh demo workspace in an empty temporary directory
and runs every `sh` block in order against the built binary. It then checks:

- that each command exits with the status the walkthrough states;
- that each quoted output still matches what the command prints;
- that each refusal and each dry run leaves the canonical journal
  byte-identical.

A block that is not run must start with a `Schematic` line. These
walkthroughs have none: every example in them is executed. A walkthrough
that drifts from the binary fails the test. The failure names the file and
line of the block, the quoted lines that no longer match, and the full
output.
