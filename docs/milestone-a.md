# Milestone A proof

Run both synthetic cases against the same public binary from a clean checkout:

```sh
cargo build --locked -p investigation-cli
python3 scripts/prove_milestone_a.py target/debug/investigation
```

Pass `--output PATH` to the proof driver to retain the workspaces and saved JSON briefs. The driver uses receipts to get the real IDs for later operations. The CI check also runs the quickstart directly from the README. [Timeout proof](../scripts/scenario_a_proof.py) · [Architecture proof](../scripts/scenario_c_proof.py)

## Observed results

| Case | Result and exact source path |
| --- | --- |
| Timeout | A slow/control pair shared the recorded workload and route. The baseline held a connection for 28,007 ms, the scoped toggle lowered the recorded hold, and reversal returned it above the failure boundary. The incompatible-build scheduler run could not promote the candidate, and the unmapped clock remained invalid. The accepted finding applies to the recorded synthetic build family only. The reviewer could follow the observation to its pinned artifact and select the exact captured callback passage by JSON pointer. |
| Architecture | The database-backed queue was a conditional choice at the original 300-second recovery requirement. Independent verification of acknowledged-job loss and peak-traffic cost remained open. The active criterion changed to 60 seconds, deferring acceptance and invalidating the earlier comparison and review basis. A successor decision remained proposed, and the case closed inconclusive. The retained JSON pointer `/results/database_queue/recovery_p95/value` still yielded the reported 110 seconds. The case contained zero Hypotheses, Inquiries, and Conclusions. |

Both material cases recorded a Retrospective. A second closure in the timeout proof exercised the non-material waiver with a reason. A crash-model test interrupted 80 points in the same-batch retrospective and closure path and checked that a restart retained both records or neither.

The independent read-only resume sessions started with the public binary and committed scratch cases on 2026-11-22. The timeout reader recovered the scoped mechanism, the recommendation to test a compatible scheduler-only toggle, and the 28,007 ms observation at revision 1. The architecture reader distinguished the historical conditional decision from the current proposed successor. They identified the 60-second hard criterion, followed the source selector to 110, and proposed an independent failure and recovery test measuring p95 and acknowledged-job loss. The exercise exposed a missing case-level caveat for proposed conditioned decisions, which the final brief now surfaces.

The independent handoffs were produced by the Rust 1.98.1 build of the implementation integrated in `b25fed4`. The copied executable used for those reads had SHA-256 `423e957e7e09bd8cb9004efa92198c3694b4ebf7ddf4fbcccc328d15e1b256b1`. The proof driver reruns the cases on the checkout's current executable without assuming the same generated identifiers.

The paired reviewer walk passed on a single debug executable built with Rust 1.98.1 (`48a229cea`) and Cargo 1.98.1 (`797e8a9bc`). That executable had SHA-256 `d5aa9511703e75f500b689027ab9112644dc9d826f92e340b5e5e64c700498e8`. The walk reads the conclusion and the historical decision at title, summary, and full depth. It follows their exact Observation and Artifact revisions to the retained callback bytes `[801, 835)` and the recovery p95 JSON pointer. It also reads the architecture assumption that traffic stays below 2,500,000 jobs per day. The timeout finding records the unresolved scheduler contribution and its synthetic scope as caveats.

## Invariant evidence

| Invariant | Witness |
| --- | --- |
| Evidence is not a verdict | An artifact read labels its content as untrusted evidence. Both verdicts require a separate attributed review and exact closure. |
| An invalid experiment is not a negative result | The timeout proof retains the clock-confounded run and rejects the different-build toggle as causal support. |
| Later changes invalidate stale reviews | The architecture proof changes 300 to 60 seconds, sees the exact stale comparison and decision, and reads the retained original evidence. |
| Formatting cannot produce independent confirmation | The ingest test verifies original bytes, identical digest for identical bytes, and one acquisition for a reformatted retry of the same manifest and command. A separately recorded acquisition still exposes the same digest. |
| Losing a response cannot duplicate a commit | The fresh-process receipt test discards the input, rebuilds SQLite, and recovers the original case, artifacts, digest, and receipt. Changing bytes under that key conflicts. |
| Missing bytes stay missing | Integrity checks identify the exact dependent records. The evidence reader rejects missing expected bytes, and promotion checks reopen the retained content before acceptance. |

The cases use synthetic local evidence and attributed judgments. They do not establish external authenticity, production behavior, physical power-loss guarantees, or superiority over plain notes. The controlled paired pilot and schema freeze are subsequent gates. Detailed test boundaries are in the support matrix and the alpha decisions. [Support matrix](support-matrix.md) · [Alpha decisions](adr/0005-milestone-a-qualification.md)
