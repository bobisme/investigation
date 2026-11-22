# ADR 0001: Canonical file-per-event journal

Status: accepted for Milestone A; 2026-11-22.

Canonical state is immutable transaction files and content-addressed artifact bytes. SQLite is a disposable, rebuildable index; Markdown is a projection. Events are UTF-8 JSON named by sequence and hashed as exact stored bytes with SHA-256. The next event records that hash. Version events from the first event and upcast on read. Gaps, forks and digest failures are integrity errors.

We choose this over canonical SQLite because investigators must be able to inspect, grep, rsync and independently check event bytes with ordinary tools. SQLite would reduce transactional implementation risk; this choice obliges fault injection, real-filesystem crash tests, concurrency tests and clean-machine restore. Compilation or a passing fake alone is not proof of durability.

Publish immutable blobs before the referencing event. Write and fsync files, publish without replacement on the same filesystem, and fsync directories. The event directory sync is the commit boundary. Every fsync error is fatal: report unknown commit status where possible, stop writes and verify recovery before accepting more. Retry with the same command ID. ADR 0002 defines scope; plan §10.2 defines the protocol.
