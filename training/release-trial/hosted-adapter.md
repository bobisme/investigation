# Operator adapter for hosted CLIs (W14.08a)

This page is a recipe. The repository ships no host-specific adapter (see [Backends](protocol.md#backends)). The operator's adapter for the sealed run lives outside the repository, in `~/.local/share/investigation-evaluator/adapter/`. This page says how it is laid out, how to check and pin it, and what the development dry run showed. The adapter only ever sees the conversation the harness sends it, so it works the same for any task content.

## Layout

| File | Role |
|---|---|
| `adapter.py` | The adapter. It handles one request per process and keeps no state between calls. |
| `hosts/claude-2.1.332` | A copy of the Claude Code binary, read-only. |
| `hosts/codex-0.172.0/` | A copy of the Codex release tree, read-only. |
| `hosts/codex-0.172.0.json` | The size and SHA-256 of every file in that tree. |
| `verify_clean.py` | The credential check for a run's outputs. |

The binaries are copies so that an automatic update of either CLI cannot change the host during a trial. The command line names the binaries and the manifest, so the plan pins their digests together with the adapter's (see [Pinning the adapter](protocol.md#pinning-the-adapter)). The adapter checks the rest of the Codex tree against the manifest: by size and mode on every call, and by SHA-256 at `describe`.

## Models

| Plan name | Host | Model | Effort |
|---|---|---|---|
| `haiku` | `claude -p` | `claude-haiku-5-5` (pinned; the alias resolved to it on 2026-11-25) | `low` |
| `gpt-6-luna` | `codex exec` | `gpt-6-luna` | `low` |

The adapter uses each host as a completion engine. The host gets no tools of its own:
- Claude runs with `--tools ""`, an empty strict MCP configuration, no setting sources and no session persistence.
- Codex runs with its shell and agent tools disabled, a read-only sandbox, and its base instructions replaced.

The host writes the agent's next message as JSON: content plus calls to the tools the harness offered. The adapter converts that JSON into the protocol reply. A reply that does not parse is retried once. A Claude reply must report the pinned model and no other.

Temperature, seed and a per-response output cap cannot be set on either host, so `options` has no effect. The `describe` metering statement says so, and the plan records that statement.

## Sandbox and credentials

Each host call runs in its own bubblewrap sandbox. The sandbox has:
- a read-only `/usr` and the network files of `/etc`;
- the network;
- an empty `/work`, a tmpfs `/tmp` and a tmpfs home;
- a cleared environment.

The login reaches the sandbox only through a pipe (`bwrap --file`) into the tmpfs home. No copy of a credential is written to disk. Only the current access token is passed, never a refresh token, so a host cannot rotate the owner's login. The adapter refuses a token that expires within 15 minutes. Everything it writes is scrubbed of the token values first.

## Checking and pinning

The command line, where `A=~/.local/share/investigation-evaluator/adapter`:

```sh
/usr/bin/python3 -I $A/adapter.py --claude $A/hosts/claude-2.1.332 \
  --codex $A/hosts/codex-0.172.0/bin/codex --codex-manifest $A/hosts/codex-0.172.0.json
```

1. Run `release-trial.py adapter-check --backend-command '…' --model haiku --model gpt-6-luna` with exactly that line. The plan expands `$A` to the absolute path.
2. Pass the same line to `plan` as `--backend-command`. Pass one `--adapter-sha256` each for `adapter.py`, the Claude binary, the Codex binary and the manifest, using the reviewed digests recorded on bn-1ztx.
3. To get timing and token counts per host call, set `ADAPTER_LOG=/outside/repo/adapter.log` for `run`. The log never holds message content.
4. After the runs, run `/usr/bin/python3 -I $A/verify_clean.py TRIAL-DIR LOG-DIR`. It fails if any file holds a credential value of either login, or if a credential file exists under those directories or the adapter directory.

## Development dry run (2026-11-25)

The dry run used the development split, both models and the pinned adapter. It ran 11 assignments: every task type with `haiku` (the family in both arms), plus a lookup and task 7 stage 1 with `gpt-6-luna`.

- **Completion.** Every assignment ran to a recorded end: 3 completed and 8 exhausted a budget, with no harness errors. `grade`, `verify` and `verify_clean` passed.
- **Reliability.** `adapter-check` passed all 11 checks in each of three runs against the final adapter. The dry run had no format errors and no host stalls.
- **Latency.** The median `haiku` call took 2.5 s and the median `gpt-6-luna` call 9.3 s.
- **Cost.** The `haiku` runs totaled USD 0.30 at Claude's list price, which the subscription does not charge. Codex reports no price.

**Budgets bind on input.** Each turn resends the whole conversation, and the harness counts reported cached input as input. Most sessions therefore reached the 180,000 or 95,000 input-token budget after 6 to 20 turns. CLI-arm sessions reached it sooner, because CLI output is longer. The pilot's local engine reported only the prompt tokens it newly evaluated, and the budgets were sized on it. Before a sealed run, the owner must decide whether the frozen budgets still mean the same thing under hosted metering.

**Codex input is inflated.** `codex exec` makes several model requests for one turn and adds about 5,000 tokens of fixed context to each. One turn with a 2,000-token prompt was metered at 18,000 to 60,000 input tokens. In an earlier dry run with the same Codex settings, a `gpt-6-luna` family run used 650,000 input tokens in 9 turns. As configured, Codex is a poor completion host under these budgets.
