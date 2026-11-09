# Roadmap and status against the design

Each part of the architecture ([docs/design](design) and the C4 diagrams in [docs/architecture](architecture)) and where it stands. "Run for real" means it ran against live GitHub and live Claude, not only in tests.

## Built

| Part | Status |
|---|---|
| Sync and registry (repository discovery, facts, ETags, webhooks) | Built; webhooks run for real |
| Postgres queue: `SKIP LOCKED` claim, leases, heartbeat, fencing tokens, reaper, checkpoints | Built and tested with injected races |
| State machine and approvals bound to a plan or diff hash | Built; run for real |
| Triage, developer and reviewer agents; deterministic risk floor; re-score of the real diff | Built; run for real |
| Runners: PR, CI wait, merge of only the reviewed commit | Built; run for real |
| Admission control (parallel development, CI queue depth), retry and token budgets, stale tickets | Built |
| Console: setup, approvals, tickets, repositories, live 3D map, replay, health | Built |
| Chat bridge (Telegram): `/ask`, `/task`, approvals by button, outcomes announced to the reporter's group | Built; tested against a fake API only |
| Changes asked for on the pull request by collaborators send the ticket back to the developer | Built; tested against a fake source only |
| Duplicate proposals (word overlap, optional model suggestion; a person decides) | Built |
| Per-repository memory (notes written by people) and `.tasklane.yml` that can only tighten | Built |
| Two installs on one repository (ownership claim) | Built; run for real |
| Agent sandbox: Docker container, egress allowlist proxy, optional screenshots with Obscura | Built; Obscura tested with a stand-in only |
| A GitLab ticket source (issues and merge requests) | Built; tested against a fake GitLab only |
| A generic engine plug-in for any command-line coding agent (same sandbox and checks) | Built; tested with a stand-in agent, not yet with Codex, Gemini or Aider |
| Plug-in protocol with protocol version check; source, engine and chat plug-ins; a plug-in guide and a tested template | Built |
| Operations: `tasklane doctor`, Prometheus `/metrics` (opt-in), a `runs` record of every model run, cleanup of stale tickets and orphaned containers | Built |
| Threat-model tests (prompt-injection set for every role, lease races, approval binding) | Built |

## Next

| Part | Notes |
|---|---|
| More ticket sources (Gitea, Jira) | GitLab exists; others are the best way to contribute |
| Reviewer agent that looks at screenshots of the change | Needs a runner step that starts the preview at the pull request's head |
| Merge queue support (drive GitHub's native queue instead of refusing) | Today a repository with a merge queue fails the ticket with a clear message |
| Update with rollback, Kubernetes manifests, a public container image | Operations |

## Not in this repository

Some parts of the original design (the ticket gateway's attachments and push notifications, the device tool broker and its adapters, and managed shared memory) are scale and governance features that are not part of the free Community edition. The plug-in interfaces they would use are public (`docs/design/03-plugin-interfaces.md`).

## Not yet run for real

Telegram, more than one approver, the generic engine with a real non-Claude agent, a real GitLab, the real Obscura binary, a repository that uses GitHub's merge queue.
