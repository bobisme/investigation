# Platform and durability support matrix

Initial policy, 2026-11-22. **No configuration is yet qualified for durable writes.** The intended first target is Linux on a local filesystem, subject to the evidence below. This is not a claim based on compilation.

| OS / filesystem | Status | Evidence / restriction |
| --- | --- | --- |
| Linux, local filesystem (specific filesystem and mount) | Intended target; unqualified | No passing configuration-specific crash, concurrency and restore evidence recorded yet. |
| macOS or Windows, any filesystem | Untested; unqualified | Publication, locking and directory-sync behavior have not been qualified. |
| Network filesystems | Refused for canonical writes | Locking and publication guarantees cannot be assumed; detect and refuse at initialization and report in doctor. |
| Cloud-synchronized canonical directories | Unsupported | Synchronization is not local transactional coordination. |
| FUSE and other uncertain filesystems | Untested or refused according to verified capabilities | Do not infer durable semantics; test-only fault-injection overrides must not enable ordinary production writes. |

Before qualifying a concrete OS, kernel, filesystem, mount and storage configuration, link dated runs covering the storage fake crash matrix, multi-process locking and conflicts, real-filesystem power-loss-style testing where available, and clean-machine archive restore. Record publication primitive, lock semantics, directory fsync behavior, tool versions and unavailable tests. A failed or unavailable required test remains visible. `doctor` should distinguish qualified, untested and refused environments and name missing evidence; this is future behavior, not a claim of current implementation.
