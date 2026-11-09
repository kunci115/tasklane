# tasklane

Ticket-driven AI agent pipeline: a problem is reported, triaged, approved by a human, fixed by an agent in an isolated workspace, checked by CI and a reviewer, and merged to a dev/staging branch. Humans stay in charge of approvals and of production.

**Status: design phase plus a compiling skeleton. The pipeline does not run yet.** The architecture is drawn as C4 diagrams in [`docs/architecture`](docs/architecture); the design document is being written in [`docs/design`](docs/design).

## Stage 1 scope

- Ticket source: GitHub only. Agent engine: Claude only.
- Ticket source, agent engine, notifier, chat channel and device adapters are fixed plug-in interfaces, so other sources and engines can be added later without touching the core.
- One install = one organization. Install with `docker compose up`, then a setup wizard (planned).
- Safe defaults: repos start read-only; nothing merges without approval; production is never touched by the system.

## Layout

| Path | What |
|---|---|
| `crates/tasklane-core` | Pure domain rules: ticket states, who may move them, risk levels (tested) |
| `crates/tasklane-plugin` | Wire protocol for plug-ins (JSON lines over stdio) |
| `crates/tasklane-server` | HTTP server (health endpoint and console placeholder) |
| `plugins/` | Plug-in programs, any language; `engine-claude` is a Python skeleton |
| `migrations/` | Postgres schema |
| `docs/` | Architecture diagrams and design documents |

## Develop

```bash
cargo test --workspace                          # Rust: core, protocol, server
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
