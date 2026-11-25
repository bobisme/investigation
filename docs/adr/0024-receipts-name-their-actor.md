# ADR 0024 – Every write's receipt names its actor and where it came from

Accepted with bn-2wct (DF.01); 2026-11-25. Refines plan §9.2 (selection and defaults) and the first-use actor setting (bn-6vlm). [Wire](../../crates/investigation-core/src/wire.rs) · [Selection](../../crates/investigation-cli/src/selection.rs) · [Contract](../schema-v1.md#attribution) · [First use](../first-use.md#actor-named-once-bn-6vlm) · [Dogfood note](../../notes/dogfood/2026-11-24-perf08.md)

**Problem.** In the W14.10 dogfood (friction F1), `investigation init DIR --actor agent:dogfood` recorded a machine-local actor, and the next command, `start`, ran under a launcher that had set `AGENT=investigation-dev`. The case was attributed to `investigation-dev`. That is the documented precedence: `AGENT` comes before the local actor. But nothing in the receipt said which actor the write used or why. The only hint was a `next_write` with `--actor investigation-dev`. The workspace had to be re-created.

**What the plan says.** Line 893: "`--actor` selects an attributed actor; default can come from workspace-local configuration or the established `AGENT` convention. Never interpret this as verified identity." The plan sets no order between the defaults. bn-6vlm made the local actor the last resort, so that it changes only calls that were refused before.

**Decision.**

1. *The precedence is kept:* `--actor` (equal to the request's actor when both are given), the request's actor, `investigation.toml`, `AGENT`, then `local/actor.toml`. Making the local actor beat `AGENT` would change the attribution of writes that work today.
2. *Every write's receipt says which actor it used.* The core `Receipt` gains the optional `attribution` object: `actor` and `source` (`flag`, `request`, `workspace_config`, `environment`, `local_actor`), and `overrides_local_actor`. The text form has one `Actor: ACTOR (from SOURCE)` line. The ingest, producer-run and capture receipts carry it through the `Receipt` they flatten or embed.
3. *One place sets it.* `select_actor` resolves the actor and records the attribution for the invocation. `main` stamps the response once, after the command returns: the journal receipt it is or embeds (`commit`, `consultation_receipt`), or the top level of a write with no journal receipt. No receipt constructor carries it, and the store never journals it, so stored receipts and their replays are unchanged. Reads that name an actor (`receipt KEY`, the schema form, a session read) use `read_actor`, which records nothing.
4. *AGENT over the init actor is said once.* When `AGENT` decided the actor and `local/actor.toml` in the same workspace names a different one, `overrides_local_actor` names it, and the receipt carries the warning `actor.agent_overrides_local: ...` with the `--actor` that uses the recorded actor: one `Warning:` line in text. An `AGENT` that names the recorded actor overrides nothing and gives no warning. An unreadable `local/actor.toml` gives no warning and does not fail a write that `AGENT` attributes, as before.

**Rejected alternatives.**

- *Let the local actor beat `AGENT`.* This fixes F1 by changing whose work every `AGENT`-launched write is. The plan does not ask for it, and bn-6vlm kept the setting to calls that were refused before.
- *Refuse a write when `AGENT` and the local actor differ.* An agent would then be blocked by a setting someone else left on the machine. The warning names the fix, and the write stays correct under the documented order.
- *Add the field to each receipt constructor.* There are four `Receipt` constructors in the store and core and more receipt types around them. Each new write path would have to remember to set the field. Stamping at the response covers every write path, including future ones.

**Consequences.**

- *Compatibility.* `attribution` is an optional property of `receipt.schema.json` and of the capture receipts' `commit` (closed objects, ADR 0007). In `ingest-receipt.schema.json` and `producer-run-receipt.schema.json`, which are open objects, it is unconstrained (`true`), as the compatibility check requires for a new property there; `receipt.schema.json` gives its shape. `ActorSource` is a closed enum, because a v1 receipt reader must know every value: a new source needs a new version. No new error code. A Rust reader using `investigation_core::wire::Receipt` accepts receipts with or without the field.
- *Warnings.* `actor.agent_overrides_local` is a new warning. The bn-6vlm warning for the local actor has the same text, but it now comes from the stamp, so every write's receipt that used the local actor carries it, not only `record`, `start` and consultations.
- *Text.* Write receipts in text gain the `Actor:` line before `Next:`.

**Tests.** Each fails without its fix (bn-2wct report). In `crates/investigation-cli/tests/first_use_actor.rs`: `every_write_receipt_names_its_actor_and_where_it_came_from` covers request, flag, dry run, `AGENT` over the init actor in text, `AGENT` equal to it, `investigation.toml`, and a lookup with no attribution. `the_onboarding_init_records_the_actor_and_a_start_without_actor_succeeds_attributed_to_it` covers the local actor and `AGENT` overriding it in JSON. It also covers a malformed `local/actor.toml` under `AGENT`. `capture.rs` covers the capture `commit`, `lessons.rs` the `consultation_receipt` and `note_snapshot.rs` a write with no journal receipt.
