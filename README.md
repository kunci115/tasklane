<p align="center">
  <img src="crates/tasklane-server/assets/brand/app-icon.svg" alt="tasklane logo" width="112" height="112">
</p>

<h1 align="center">tasklane</h1>

<p align="center"><strong>Open-source AI agent orchestration for GitHub issues.</strong><br>
Open an issue, get a reviewed pull request. A person approves anything risky.</p>

<p align="center">
  <a href="https://github.com/kunci115/tasklane/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/kunci115/tasklane/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue.svg"></a>
  <img alt="Core: Rust" src="https://img.shields.io/badge/core-Rust-orange.svg">
  <a href="https://github.com/kunci115/tasklane/releases"><img alt="Release" src="https://img.shields.io/github/v/release/kunci115/tasklane?include_prereleases&label=release"></a>
  <img alt="Self-hosted" src="https://img.shields.io/badge/self--hosted-yes-brightgreen.svg">
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#autonomy-when-the-agents-act-alone-and-when-they-ask">Autonomy</a> ·
  <a href="#safety-model">Safety model</a> ·
  <a href="docs/operations.md">Operations guide</a> ·
  <a href="docs/design">Design</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

<p align="center">
  <img src="docs/demo.gif" alt="tasklane console: a ticket waiting for approval with the exact diff, then the live 3D map of agents" width="860">
</p>

**tasklane** is a self-hosted pipeline that turns GitHub issues into reviewed pull requests with an AI coding agent, and keeps a human in charge of every risky step. It watches your repositories, plans the fix, asks a person to approve when the change is risky, lets the agent write the change in an isolated git workspace, runs your CI, has a second agent review the diff, and merges **only the exact commit that was reviewed**. It never touches production.

It is the orchestration layer around an agent, not another agent: a durable job queue, deterministic risk rules, approvals, retries, an audit trail and a console. The agent (Claude today) is one replaceable part.

## Why tasklane

- **You stay in control.** Risk levels come from fixed rules (migrations, auth, secrets, CI and deploy config, payments, personal data). The model can only raise a level, never lower it. A timer never approves anything.
- **Approvals mean something.** An approval is bound to a hash of the exact plan or diff. Change one line and the approval no longer applies.
- **The agent holds no keys.** The daemon pushes and merges with short-lived, single-repository tokens. The agent runs read-only, or in a locked-down Docker container with no secrets.
- **Work survives crashes.** A Postgres queue with `SKIP LOCKED` claims, leases, heartbeats and fencing tokens means a job never runs twice and a dead worker's job is picked up again.
- **Two installs do not collide.** An install claims a repository on GitHub with an atomic ref; a second install on the same repository stays out of the way.
- **Bring your own agent or tracker.** Ticket sources, agent engines, notifiers and chat channels are separate programs speaking JSON lines over stdio. Python plug-ins ship today; any language works.
- **One daemon, one database, no build step.** The web console is served by the daemon: setup, approvals, a live 3D map of agents at work, and replay.

## How it works

```mermaid
flowchart LR
    I[GitHub issue] --> T[Triage agent<br/>read-only]
    T --> R{Risk level<br/>from fixed rules}
    R -- low --> D
    R -- needs a person --> A[Approval of the<br/>exact plan or diff]
    A --> D[Developer agent<br/>isolated git worktree]
    D --> C[Re-check the real diff]
    C --> P[Pull request]
    P --> CI[Your CI]
    CI --> V[Reviewer agent]
    V --> M[Merge the<br/>reviewed commit]
```

1. **Triage.** A read-only agent reads the issue and the code and writes a plan. Strangers' issues and anything near protected paths always wait for a person.
2. **Develop.** After approval (or automatically for low-risk work in repositories you let it), the developer agent edits files in a throwaway git worktree. It cannot push.
3. **Re-check.** tasklane scores the real diff with the same fixed rules. A protected path or a much bigger change than planned sends it back for approval of that exact diff.
4. **Pull request, CI, review.** tasklane opens the PR, waits for your CI, and a second agent reviews the diff. Requested changes go back to the developer, within a retry budget.
5. **Merge.** Only the reviewed head commit is merged, to your development branch, and only if branch rules pass on the server. Promotion to production is outside the system.

## Quickstart

You need Docker with Compose, a GitHub account (or organization) and either an Anthropic API key or a Claude subscription.

```bash
git clone https://github.com/kunci115/tasklane && cd tasklane
export POSTGRES_PASSWORD=$(openssl rand -hex 16)
docker compose -f docker-compose.prod.yml up -d --build
docker compose -f docker-compose.prod.yml logs tasklane     # the admin token is printed once
```

Open <http://localhost:8080>, sign in with that token, and follow the **Getting started** card:

1. **Connect GitHub.** One click opens GitHub with the App already filled in; press Create, then Install and pick the repositories. No key files, no config file.
2. **Connect the agent.** Paste an API key, or a Claude subscription token from `claude setup-token`. Choose how the agent is isolated and press **Run the test**.
3. **Choose repositories.** New ones start in *triage*: every ticket's plan waits for your approval before any code is written. Switch one to *full* when you trust it.
4. Open an issue. It appears on the Overview within a minute.

Run `tasklane doctor` to check an installation (database, plug-ins, git, Docker). Put a TLS reverse proxy in front of port 8080 if the console is reachable from outside. Full setup, environment variables, backups and upgrade notes: [docs/operations.md](docs/operations.md).

| | | |
|---|---|---|
| ![Getting started](docs/screenshots/getting-started.png) | ![Connect GitHub](docs/screenshots/connect-github.png) | ![Connect the agent](docs/screenshots/connect-agent.png) |
| First start: a checklist says what to do next. | One click creates the GitHub App. | A key or subscription token, an isolation choice, and a test. |

## The console

Served by the daemon itself, responsive (laptop, tablet, phone), light and dark, installable as an app from Chrome or Safari.

| | |
|---|---|
| ![Approve a change](docs/screenshots/approve-a-change.png) | ![tasklane live 3D map of AI agents working on GitHub issues](docs/screenshots/live-map.png) |
| **A decision:** the exact diff, why a person is needed, approve or reject. | **Live map:** a 3D map you can turn and zoom. Agents on one tier, repositories on another; agents glow while they work. |
| ![tasklane replay of agent activity](docs/screenshots/replay.png) | ![Repositories](docs/screenshots/repositories.png) |
| **Replay:** play back the last hours at up to 1200x. Idle time is skipped. | **Repositories:** choose how far tasklane may go in each, with search and paging. |

Also: a ticket list with search and paging, a plain-sentence timeline per ticket, people and roles, chat groups, an activity feed, a health checklist and a pause-all switch. The demo data in the screenshots is invented.

## Autonomy: when the agents act alone and when they ask

You decide per repository. In `full` mode a low-risk ticket from a collaborator goes from issue to **merged pull request with nobody pressing anything**, as long as CI is green on the exact commit and the reviewer agent approves it. Anything riskier stops and asks a person, and the model can raise that bar but never lower it.

| | Acts alone | Asks a person |
|---|---|---|
| Mode | `full` repository | `triage` mode, or `always_ask: true` |
| Author | collaborator, member, owner | a stranger, always |
| Text | no risk rule matched (L1) | migrations, login/permissions, deploy (L2); secrets, payments, deleting data, personal data (L3, owner only) |
| Diff | no protected path, within the size limit | CI/deploy config, `migrations/`, `.env`, keys; too large |
| Review | the reviewer agent approves the commit | the reviewer says "escalate" |

When something goes wrong (CI fails twice, three failed attempts, token budget used up) it stops and hands the ticket to a person. A timer never approves anything. Full rules, who may approve what, and every setting that turns the dial: **[docs/autonomy.md](docs/autonomy.md)**.

## Safety model

| Risk | Control |
|---|---|
| The agent does something you did not intend | Fixed risk rules on the real diff; protected paths always need a person; approvals bound to a diff hash |
| Prompt injection in an issue or comment | Untrusted text stays inside delimited blocks; the agent has no secrets, no network beyond the model API, and a fixed tool list; strangers' issues always wait for approval |
| Leaked credentials | The agent never sees tokens; the daemon uses short-lived per-run tokens scoped to one repository |
| Runaway cost | Per-ticket token budget, attempt limits, a bounded queue, a pause-all switch |
| A merge nobody reviewed | The merge step takes the exact reviewed commit; a retry never skips review |
| Two installs acting on one repository | Atomic ownership claim on GitHub; the loser skips the repository |
| Stale tickets | Tickets nobody answers are closed as stale, never approved by time |

The agent can run **unsandboxed** only if you explicitly opt in; the default is refusal. Read the [threat model](docs/design/04-threat-model.md) and [SECURITY.md](SECURITY.md) for the residual risks, including what a model can still get wrong in a diff.

## Features

| | |
|---|---|
| Ticket source | GitHub Issues through a GitHub App; GitLab issues and merge requests (experimental) |
| Agent engine | Claude through the Claude Code CLI (API key or subscription token), or **any command-line coding agent** (Codex, Gemini CLI, Aider, ...) through the generic engine plug-in |
| Isolation | Read-only agents for triage and review; developer in a git worktree, optionally inside a locked-down Docker container whose only way out is an allowlist proxy to the model API |
| Roles | Owner and team approver, with different levels they may approve |
| Review loop | Collaborators' comments and change requests on the pull request are given back to the developer agent; the new change is re-scored, tested and reviewed again |
| Chat | Telegram bot for questions and `/task`, approvals by button, and the outcome of each ticket reported back to the group that raised it |
| Console | Setup, approvals, tickets (search and paging), repositories, people, activity, live map, replay, health |
| Per-repository control | Mode (`watch`, `triage`, `full`), protected paths, agent instructions, notes for the agents, and a `.tasklane.yml` that can only tighten the rules |
| Duplicates | A new ticket that repeats an earlier one is held for a person to confirm (optional model suggestion) |
| Cost and limits | Model tokens counted per ticket with a budget, attempt and wall-time limits, a cap on parallel development and on pull requests waiting for CI |
| Seeing the result | Optional screenshots of the app the agent is building, with Obscura, inside the sandbox |
| Operations | `tasklane doctor`, Prometheus `/metrics` (opt-in), `/healthz` and `/readyz`, automatic cleanup of stale tickets and orphaned containers |
| Queue | Postgres, `SKIP LOCKED`, leases, heartbeats, fencing tokens, reaper |
| Extension points | Plug-ins for ticket source, agent engine and chat channel, with a [guide](docs/plugins.md) and a tested template |

## FAQ

**Is this an agent framework or a product?** A product you run: one daemon, one Postgres, a console. The plug-in protocol is the extension point if you want to add a ticket source or an engine.

**How is it different from running a coding agent in CI?** tasklane is the layer around the agent: the queue, the risk rules, the approvals, the review step, retries, an audit trail and a console. You can swap the agent without changing how work is controlled.

**Can it merge to production?** No. It merges to a development branch after CI and review. Promotion is yours.

**Does every developer need a Claude account?** No. One install uses one agent credential for all tickets. Use an API key for a team; a subscription token is one person's plan.

**Which models and trackers work today?** Claude and GitHub Issues. Both sit behind plug-in interfaces so others can be added without touching the core.

**What does it cost?** The software is free (Apache-2.0). You pay your model provider directly for tokens.

## Project status

tasklane is **pre-release (0.1.0-alpha.1)**. Expect rough edges, and read the next two points before you rely on it.

- **Verified:** the whole pipeline from issue to merge runs in tests against a real Postgres and git. Two supervised runs against live GitHub and live Claude (one-click App creation, subscription token, Docker sandbox) took issues to merged pull requests, and two installs on one repository were run side by side. They found and fixed seven bugs, listed in the [CHANGELOG](CHANGELOG.md).
- **Not yet run for real:** Telegram (fake API only) and more than one approver. GitHub webhooks have run for real (issue and comment events through a tunnel).
- **Please do your own supervised dry run** on a throwaway repository before you point it at real work. [docs/operations.md](docs/operations.md) lists the limits honestly.

## Built with

| Part | What |
|---|---|
| Core | Rust (2021 edition): [tokio](https://tokio.rs) (async runtime), [axum](https://github.com/tokio-rs/axum) 0.8 (HTTP), [sqlx](https://github.com/launchbadge/sqlx) 0.8 (PostgreSQL, embedded migrations), serde and serde_json, thiserror, sha2, hmac, subtle (constant-time comparison), base64, getrandom |
| Database | PostgreSQL 14+ (the queue, state and audit trail live here; no Redis, no broker) |
| Plug-ins | Python 3 standard library only (no pip dependencies): `source-github`, `engine-claude`, `chat-telegram` |
| Agent | [Claude Code](https://docs.claude.com/en/docs/claude-code) CLI, driven by the `engine-claude` plug-in |
| Console | Server-rendered HTML from the daemon, plain CSS and JavaScript, a canvas-drawn 3D map; no frontend framework and no build step. Fonts bundled: Inter, JetBrains Mono, Chakra Petch ([docs/THIRD_PARTY.md](docs/THIRD_PARTY.md)) |
| Packaging | One Docker image (Debian bookworm, Node 22 for the CLI) and Docker Compose files |

## Documentation

| | |
|---|---|
| [Writing a plug-in](docs/plugins.md) | The protocol in one page, a tested template, how to register a plug-in |
| [Roadmap and status](docs/roadmap.md) | Each part of the design: built, next, and what has run for real |
| [Autonomy and escalation](docs/autonomy.md) | When agents act alone, when a person is needed, and every setting that turns the dial |
| [Operations guide](docs/operations.md) | Install, configuration, backups, upgrades, sandbox, two installs on one repository |
| [Design documents](docs/design) | State machine, schema, plug-in interfaces and protocol, threat model |
| [C4 architecture](docs/architecture) | Context, containers and components as diagram sources |
| [CHANGELOG](CHANGELOG.md) | What changed and what each real run found |

## Repository layout

| Path | What |
|---|---|
| `crates/tasklane-core` | Pure domain rules: ticket states, who may move them, risk levels, protected paths |
| `crates/tasklane-queue` | Postgres job queue |
| `crates/tasklane-orchestrator` | Worker that runs triage, develop, PR, CI, review and merge by calling plug-ins |
| `crates/tasklane-plugin` | Plug-in wire protocol (JSON lines over stdio) |
| `crates/tasklane-server` | The daemon: HTTP API, web console, chat bridge |
| `plugins/` | `source-github`, `engine-claude`, `chat-telegram` (Python) |
| `migrations/` | Postgres schema |

## Contributing

Contributions are welcome, especially new ticket-source and agent-engine plug-ins (start from [docs/plugins.md](docs/plugins.md) and the tested template in `plugins/_template`) and real-world test reports. See also [CONTRIBUTING.md](CONTRIBUTING.md).

```bash
TASKLANE_TEST_DATABASE_URL=postgres://tasklane:tasklane@127.0.0.1:5432/postgres \
  cargo test --workspace                        # Rust; database tests need Postgres
python3 -m unittest discover -s plugins/engine-claude
docker compose up -d db                         # Postgres with the schema loaded
TASKLANE_BIND=127.0.0.1:8080 cargo run -p tasklane-server
```

Found a security problem? Do not open a public issue; follow [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE). Third-party fonts are listed in [docs/THIRD_PARTY.md](docs/THIRD_PARTY.md).
