# Architecture decisions

| ID | Decision |
| --- | --- |
| [0001](0001-canonical-journal.md) | Canonical event encoding, hashes, and durable publication |
| [0002](0002-chain-scope.md) | Per-case chains and workspace command identity |
| [0003](0003-a-gate.md) | Paired pilot and iteration threshold |
| [0004](0004-product-boundaries.md) | Executable name, material closure, and supported durability claims |
| [0005](0005-milestone-a-qualification.md) | Alpha evidence, import, recovery, and qualification boundaries |
| [0006](0006-a-gate-catalog-disposition.md) | Pilot outcome and record-type disposition |
| [0007](0007-v1-schema-freeze.md) | Frozen public v1 contracts and compatibility |
| [0008](0008-method-pack-security.md) | Method pack security and policy inputs at promotion gates |
| [0009](0009-incremental-vault-render.md) | Incremental vault render and the basis an unchanged note keeps |
| [0010](0010-verified-head-checkpoints.md) | Verified-head checkpoints for journal verification |
| [0011](0011-case-scoped-verification.md) | Case-scoped verification: a command verifies the cases it reads |
| [0012](0012-batched-vault-durability.md) | Vault publication in durability batches, and unchanged files known by their status |
| [0013](0013-archive-restore-and-fork.md) | Archive restore and fork: verified staging, marker-last publication, and fork identity |
| [0014](0014-migration-framework.md) | Workspace format in the marker, ordered migrations with a verified backup, and historical fixtures |
| [0015](0015-index-durability.md) | Index durability: the fully synced rollback journal stays, a rebuild syncs once and never inherits an old journal |
| [0016](0016-byte-canonical-journal-files.md) | Journal files are the exact bytes this program writes, and `repair journal` restores witnessed bytes |
