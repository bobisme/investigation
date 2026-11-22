# investigation

Project type: cli
Tools: `bones`, `maw`

<!-- Add project-specific context below: architecture, conventions, key files, etc. -->

<!-- edict:managed-start -->## Edict Workflow

Tools teach their own commands: bones `bn tldr` · maw `maw tldr` (`maw --help`) · edict `edict protocol --help`. Some tool docs are out of date: where a tool's quick start differs from the Rules below, the Rules win. Identity: `$AGENT`, set by the launcher; manual sessions use `<project>-dev`.

Layout: the repo root is the trunk (the `default` workspace); agent workspaces live in `.maw/workspaces/<name>/`.

### Rules

- **Track all work in a bone.** Create it before you start (`bn create`), move it `open` → `doing` → `done`, and post progress comments for crash recovery. See [update.md](.agents/edict/update.md).
- **Edit only in a maw workspace** named after the bone: `maw ws create <bone-id> --from main`, then work in `.maw/workspaces/<bone-id>/`. Never edit the trunk at the repo root directly, and never create git branches: maw workspaces replace them. See [start.md](.agents/edict/start.md).
- **Run each tool in its place.** Run commands in a workspace with `maw exec <ws> -- <cmd>`. Run `bn` directly at the repo root.
- **Never merge or destroy `default`.** It is the merge target.
- **Run the edict protocol at each transition:** `edict protocol resume|start|review|finish|merge|cleanup … --agent $AGENT`, then run the steps it prints, in order. If it exits 1, follow the matching workflow doc below.
- **Reviewed work merges only through `edict protocol merge <ws> --message "feat: …"`.** A bare `maw ws merge` skips the review log, the clean check and the `risk:critical` gate; use it only for work with no review. See [merge-check.md](.agents/edict/merge-check.md#the-review-log-and-the-clean-check).
- **A conflicted workspace is a normal state, not a failure:** `maw ws resolve <ws> --list`. See [merge-check.md](.agents/edict/merge-check.md#conflict-recovery).
- **Run `maw ws recover` before you conclude work is lost** or start a bone over. Destroyed workspaces keep snapshots. See [worker-loop.md](.agents/edict/worker-loop.md).
- **Run `just check` before committing**, and fix failures first. Workers do not push; the lead merges and pushes. See [finish.md](.agents/edict/finish.md).
- **Confirm before destructive actions** (deleting data, force-pushing, discarding unmerged work): ask the human first.

### Release

Install locally after releasing: `just install`

### Design Guidelines

- [CLI tool design for humans, agents, and machines](.agents/edict/design/cli-conventions.md)

### Workflow Docs

- [worker-loop.md](.agents/edict/worker-loop.md): Full worker cycle: resume, triage, start, work, review, finish
- [triage.md](.agents/edict/triage.md): Find one actionable bone and groom along the way
- [start.md](.agents/edict/start.md): Claim a bone, create its workspace, announce
- [update.md](.agents/edict/update.md): Change a bone's state and announce it
- [review-request.md](.agents/edict/review-request.md): Request a review: commit first, review range, retarget before re-request
- [review-response.md](.agents/edict/review-response.md): Handle reviewer feedback; no code after the LGTM
- [security-review.md](.agents/edict/security-review.md): Launch one dedicated security review; who sends what
- [finish.md](.agents/edict/finish.md): Close the bone, merge or hand off, release claims; conflict recovery
- [merge-check.md](.agents/edict/merge-check.md): Merge a workspace: review log, clean check, merge gates, conflicts
- [cross-channel.md](.agents/edict/cross-channel.md): Rite threads, ask-and-wait, message shape, cross-project asks
- [report-issue.md](.agents/edict/report-issue.md): Superseded by cross-channel.md
- [planning.md](.agents/edict/planning.md): Turn a spec or PRD into actionable bones
- [scout.md](.agents/edict/scout.md): Explore unfamiliar code before planning
- [proposal.md](.agents/edict/proposal.md): Propose and validate a significant change before building it
- [groom.md](.agents/edict/groom.md): Groom ready bones to improve backlog quality
- [mission.md](.agents/edict/mission.md): Missions: split a parent bone across parallel workers
- [coordination.md](.agents/edict/coordination.md): Coordinate with sibling workers inside a mission
- [preflight.md](.agents/edict/preflight.md): Validate toolchain health before multi-agent work
<!-- edict:managed-end -->
