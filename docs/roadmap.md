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
| Fix loop: red CI or a reviewer's "changes" sends the ticket back to the developer with what the failed checks said, limited by `max_dev_attempts` and `max_ci_runs`, then a person | Built; the loop ran for real, the check text is tested against a fake GitHub only |
| Evaluations of the model roles (`evals/`, 28 cases, baseline comparison) and outcome metrics (first-try merges, reviewer verdicts, tokens per role) | Built; runner and case files tested in CI. No baseline is committed yet: it needs a run against a model |
| Several model accounts with automatic switch when one runs out of usage, managed on a console page; jobs wait (no attempt used) when all are used up | Built; tested with a stand-in CLI, not with real usage limits |
| Threat-model tests (prompt-injection set for every role, lease races, approval binding) | Built |

## Next

| Part | Notes |
|---|---|
| More ticket sources (Gitea, Jira) | GitLab exists; others are the best way to contribute |
| Reviewer agent that looks at screenshots of the change | Needs a runner step that starts the preview at the pull request's head |
| Plans that list the files they will touch; a diff outside that list asks a person. Test-first as a repository rule | Idea from the "agent skills" workflow (spec, plan in small tasks, test-first, verify) |
| Merge queue support (drive GitHub's native queue instead of refusing) | Today a repository with a merge queue fails the ticket with a clear message |
| Update with rollback, Kubernetes manifests, a public container image | Operations |

## Not in this repository

Some parts of the original design (the ticket gateway's attachments and push notifications, the device tool broker and its adapters, and managed shared memory) are scale and governance features that are not part of the free Community edition. The plug-in interfaces they would use are public (`docs/design/03-plugin-interfaces.md`).

## Not yet run for real

Telegram, more than one approver, the generic engine with a real non-Claude agent, a real GitLab, the real Obscura binary, a repository that uses GitHub's merge queue.
