# tasklane

Ticket-driven AI agent pipeline: a problem is reported, triaged, approved by a human, fixed by an agent in an isolated workspace, checked by CI and a reviewer, and merged to a dev/staging branch. Humans stay in charge of approvals and of production.

**Status: pre-release (0.0.1).** The whole pipeline from issue to merge runs in tests against a real Postgres and git, and has had one supervised dry run against live GitHub and live Claude (see the CHANGELOG). Telegram, webhooks and the Docker sandbox with a valid key have not run for real. See [docs/operations.md](docs/operations.md) for setup and the honest list of limits. The architecture is drawn as C4 diagrams in [`docs/architecture`](docs/architecture); the design document is being written in [`docs/design`](docs/design).

## Stage 1 scope

- Ticket source: GitHub only. Agent engine: Claude only.
- Ticket source, agent engine, notifier, chat channel and device adapters are fixed plug-in interfaces, so other sources and engines can be added later without touching the core.
- One install = one organization. Install with `docker compose up`, then a setup wizard (planned).
- Safe defaults: repos start read-only; nothing merges without approval; production is never touched by the system.

## The console

A web console ships inside the daemon (no separate frontend, no JavaScript build). It works on a laptop, a tablet and a phone, in light and dark, and can be installed as an app from Chrome or Safari ("Add to Dock" or "Install app").

| | |
|---|---|
| ![Overview](docs/screenshots/overview.png) | ![Approve a change](docs/screenshots/approve-a-change.png) |
| **Overview:** what needs you, what is moving, a live pipeline board. | **A decision:** the exact diff, why a person is needed, approve or reject. |
| ![Live map](docs/screenshots/live-map.png) | ![Replay](docs/screenshots/replay.png) |
| **Live map:** repositories, tickets and agents as a graph. Agents glow while they work and a line shows which ticket each one has. | **Replay:** play back the last hours at up to 1200x and watch what the agents did. Idle time is skipped. |
| ![Repositories](docs/screenshots/repositories.png) | ![Health and setup](docs/screenshots/health-and-setup.png) |
| **Repositories:** choose how far tasklane may go in each, and give the agent instructions. | **Health and setup:** a checklist that tells a new user what to do next. |

Also: a ticket list with search, a timeline written in plain sentences, a progress stepper, people and roles, chat groups, an activity feed, and a pause-all switch. The demo data in the screenshots is invented.

## Layout

| Path | What |
|---|---|
| `crates/tasklane-core` | Pure domain rules: ticket states, who may move them, risk levels (tested) |
| `crates/tasklane-queue` | Postgres job queue: `SKIP LOCKED` claim, leases, heartbeat, fencing tokens, reaper (tested against a real Postgres) |
| `crates/tasklane-orchestrator` | Worker that claims a job, calls plug-ins as child processes, and moves the ticket through the state machine in one fenced transaction (triage step so far) |
| `crates/tasklane-plugin` | Wire protocol for plug-ins (JSON lines over stdio) |
| `crates/tasklane-server` | HTTP server (health endpoint and console placeholder) |
| `plugins/` | Plug-in programs, any language; `engine-claude` is a Python skeleton |
| `migrations/` | Postgres schema |
| `docs/` | Architecture diagrams and design documents |

## Develop

```bash
TASKLANE_TEST_DATABASE_URL=postgres://tasklane:tasklane@127.0.0.1:5432/postgres \
  cargo test --workspace                        # Rust; queue tests need Postgres (skipped without it)
python3 -m unittest discover -s plugins/engine-claude
docker compose up -d db                         # Postgres with the schema loaded
TASKLANE_BIND=127.0.0.1:8080 cargo run -p tasklane-server
```

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
