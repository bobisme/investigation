# Capture protocol examples (`inv.capture.v1`)

Accepted requests for `investigation capture`, in lifecycle order: `begin`, `append`, `attach`, `finalize`, `recover`. `investigation schema capture.begin` (and the other `capture.*` names) prints each file as its example.

- Put this directory in the workspace's `import_roots`; `attach.json` names `result.json` beneath it.
- `begin` returns the real handle. Replace the placeholder `handle` in the later requests with it. The placeholder is a well-formed identifier that no workspace holds.
- `crates/investigation-cli/tests/capture_schema.rs` runs these files against the built CLI, so a change that makes one invalid fails the build.
