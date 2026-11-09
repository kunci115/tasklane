<p align="center">
  <img src="crates/tasklane-server/assets/brand/app-icon.svg" alt="tasklane logo" width="112" height="112">
</p>

<h1 align="center">tasklane</h1>

<p align="center"><strong>Open-source AI agent orchestration for GitHub issues.</strong><br>
Report a problem, get a reviewed pull request. A human approves anything risky.</p>

<p align="center">
  <a href="https://github.com/kunci115/tasklane/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/kunci115/tasklane/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue.svg"></a>
  <img alt="Core: Rust" src="https://img.shields.io/badge/core-Rust-orange.svg">
  <img alt="Self-hosted" src="https://img.shields.io/badge/self--hosted-yes-brightgreen.svg">
</p>

tasklane is a self-hosted, ticket-driven **AI coding agent pipeline**. It watches your GitHub issues, plans the fix with an AI agent (Claude), asks a person to approve when the change is risky, lets the agent write the change in an isolated git workspace, runs CI, has a second agent review the diff, and merges only the exact commit that was reviewed. Production is never touched by the system.

Use it when you want an **autonomous coding agent for your issue tracker** without giving up control: every approval is bound to the exact plan or diff, agents never hold your tokens, and anything touching migrations, auth, secrets, CI or deploy config always waits for a human.

**Status: pre-release (0.0.1).** The whole pipeline from issue to merge runs in tests against a real Postgres and git, and has had one supervised dry run against live GitHub and live Claude (see the CHANGELOG). Telegram, webhooks and the Docker sandbox with a valid key have not run for real. See [docs/operations.md](docs/operations.md) for setup and the honest list of limits. The architecture is drawn as C4 diagrams in [`docs/architecture`](docs/architecture); the design document is being written in [`docs/design`](docs/design).

## What it does

```
GitHub issue -> triage (read-only agent) -> approval where needed -> develop (isolated git worktree)
             -> diff re-check -> pull request -> CI -> AI review -> merge of the reviewed commit
```

- **Human-in-the-loop by design.** Risk levels come from deterministic rules and protected paths; the model can only raise a level. A timer never approves anything.
- **Approvals bound to a hash** of the exact plan or diff, so an approval for one change cannot be reused for another.
- **Agents hold no credentials.** The daemon pushes with short-lived per-run tokens; the agent runs read-only or in a locked-down Docker sandbox.
- **Postgres job queue** with `SKIP LOCKED` claims, leases, heartbeats and fencing tokens, so work survives crashes and never runs twice.
- **Plug-in architecture.** Ticket source, agent engine, notifier and chat channel are separate programs speaking JSON lines over stdio (Python today, any language works). Stage 1 ships GitHub and Claude; others can be added without touching the core.
- **A web console** for setup, approvals, a live 3D map of agents at work, and replay. No separate frontend, no build step.

## Stage 1 scope

- Ticket source: GitHub only. Agent engine: Claude only.
- Ticket source, agent engine, notifier, chat channel and device adapters are fixed plug-in interfaces, so other sources and engines can be added later without touching the core.
- One install = one organization. Start it with one command, then connect GitHub and the agent in the console: GitHub is one click (tasklane creates the App for you), the agent is a pasted key and a test button.
- Safe defaults: repos start read-only; nothing merges without approval; production is never touched by the system.

## Getting started

```bash
export POSTGRES_PASSWORD=$(openssl rand -hex 16)
docker compose -f docker-compose.prod.yml up -d --build
docker compose -f docker-compose.prod.yml logs tasklane     # the admin token is printed once
```

Open <http://localhost:8080>, sign in, and follow the **Getting started** card. **Connect GitHub** opens GitHub with the App already filled in (press Create, then Install); **Connect the agent** takes a Claude API key or a Claude subscription token (from `claude setup-token`), an isolation choice and a test button. No config file, no key files. Details and the honest list of what has and has not been verified: [docs/operations.md](docs/operations.md).

| | | |
|---|---|---|
| ![Getting started](docs/screenshots/getting-started.png) | ![Connect GitHub](docs/screenshots/connect-github.png) | ![Connect the agent](docs/screenshots/connect-agent.png) |
| First start: a checklist says what to do next. | One click creates the GitHub App. | A key or subscription token, an isolation choice, and a test. |

## The console

A web console ships inside the daemon (no separate frontend, no JavaScript build). It works on a laptop, a tablet and a phone, in light and dark, and can be installed as an app from Chrome or Safari ("Add to Dock" or "Install app").

| | |
|---|---|
| ![tasklane console overview: tickets needing approval and a live pipeline board](docs/screenshots/overview.png) | ![Approve a change](docs/screenshots/approve-a-change.png) |
| **Overview:** what needs you, what is moving, a live pipeline board. | **A decision:** the exact diff, why a person is needed, approve or reject. |
| ![tasklane live 3D map of AI agents working on GitHub issues](docs/screenshots/live-map.png) | ![tasklane replay of agent activity](docs/screenshots/replay.png) |
| **Live map:** a 3D map you can turn and zoom. Agents sit on one tier, repositories on another; agents glow while they work and a line shows which ticket each one has. | **Replay:** play back the last hours at up to 1200x and watch what the agents did. Idle time is skipped. |
| ![Repositories](docs/screenshots/repositories.png) | ![Health and setup](docs/screenshots/health-and-setup.png) |
| **Repositories:** choose how far tasklane may go in each, and give the agent instructions. | **Health and setup:** a checklist that tells a new user what to do next. |

Also: a ticket list with search, a timeline written in plain sentences, a progress stepper, people and roles, chat groups, an activity feed, and a pause-all switch. The demo data in the screenshots is invented.

## FAQ

**Is this an AI agent framework or a product?** A product you run: one daemon, one Postgres, a console. The plug-in protocol is the extension point if you want to add a ticket source or an agent engine.

**Which models and trackers work today?** Claude (through the Claude Code CLI, with an API key or a Claude subscription token) and GitHub Issues. Both sit behind plug-in interfaces.

**Can the agent merge to production?** No. It can merge to a dev or staging branch after CI and review; promotion to production is outside the system.

**What if the agent is wrong or the issue is malicious?** Strangers' issues always need approval, protected paths always need approval of the exact diff, and the agent has no tokens and no network secrets to leak. Read [SECURITY.md](SECURITY.md) and the threat model in [docs/design](docs/design) for the honest list of limits.

**How is it different from running a coding agent in CI?** tasklane is the orchestration layer around the agent: queue, risk rules, approvals, reviews, retries, an audit trail and a console. The agent is one replaceable part.

## Layout

| Path | What |
|---|---|
| `crates/tasklane-core` | Pure domain rules: ticket states, who may move them, risk levels, protected paths |
| `crates/tasklane-queue` | Postgres job queue: `SKIP LOCKED` claim, leases, heartbeat, fencing tokens, reaper |
| `crates/tasklane-orchestrator` | Worker that claims a job, calls plug-ins as child processes, and runs triage, develop, PR, CI, review and merge |
| `crates/tasklane-plugin` | Wire protocol for plug-ins (JSON lines over stdio) |
| `crates/tasklane-server` | The daemon: HTTP API, web console, chat bridge |
| `plugins/` | Plug-in programs: `source-github`, `engine-claude`, `chat-telegram` (Python) |
| `migrations/` | Postgres schema |
| `docs/` | Operations guide, design documents, C4 architecture diagrams |

## Develop

```bash
TASKLANE_TEST_DATABASE_URL=postgres://tasklane:tasklane@127.0.0.1:5432/postgres \
  cargo test --workspace                        # Rust; database tests need Postgres
python3 -m unittest discover -s plugins/engine-claude
docker compose up -d db                         # Postgres with the schema loaded
TASKLANE_BIND=127.0.0.1:8080 cargo run -p tasklane-server
```

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md). Third-party fonts are listed in [docs/THIRD_PARTY.md](docs/THIRD_PARTY.md).

## Architecture (C4)

| Level | File |
|---|---|
| 1 Context | `docs/architecture/c4-1-context.json` |
| 2 Containers | `docs/architecture/c4-2-containers.json` |
| 3 Gateway | `docs/architecture/c4-3a-gateway.json` |
| 3 Orchestrator | `docs/architecture/c4-3b-orchestrator.json` |
| 3 Agent runtime | `docs/architecture/c4-3c-agents.json` |
| 3 Runners | `docs/architecture/c4-3d-runners.json` |
| 3 Sync and registry | `docs/architecture/c4-3e-sync.json` |
| 3 Tool broker | `docs/architecture/c4-3f-broker.json` |

The files are diagram sources (JSON); each carries its own cards for input, output, owned state and failure behaviour.

## License

Apache-2.0. See [LICENSE](LICENSE).
