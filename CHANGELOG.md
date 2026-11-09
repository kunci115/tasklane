# Changelog

## Unreleased

- Generic agent engine (`plugins/engine-cli`): run any command-line coding agent (Codex, Gemini CLI, Aider, ...) with the same Docker sandbox, read-only roles, egress allowlist, secret handling and answer checks as the Claude engine. Tested with a stand-in agent.
- Fixes from an independent security review of everything since the alpha:
  - **The developer never received feedback** (failed checks, the reviewer's notes, people's comments on the pull request): the engine ignored it. It now arrives as marked data in a `<feedback>` block that cannot close itself or change the rules.
  - `.tasklane.yml` is a protected path, so a change cannot remove a repository's own tightening without a person seeing it.
  - Comments on a pull request steer the agent only from people with write access (when GitHub can say), and the risk rules read their words too: asking for a change in refund logic needs an owner.
  - `tl-shot` refuses addresses with control characters (a second line could pass the check), and the preview only starts in a sandbox on the egress-limited network.
  - The chat no longer repeats failure details (they can carry model or workflow text); it points to the console.
- The Repositories page shows the model tokens each repository used in the last 30 days.
- Pull request feedback: a comment or a review asking for changes from a collaborator (never a stranger, never tasklane itself) sends the ticket back to the developer with their words, then through re-score, CI and review again. Checked every two minutes while a pull request is on its way.
- Optional model-assisted duplicate suggestions (`TASKLANE_DUPLICATE_MODEL=1`): the model can only name one of the 30 most recent tickets, and a person still decides.
- `docs/plugins.md`: the plug-in protocol in one page, and `plugins/_template`, a tested ticket-source plug-in (tickets in a JSON file) to copy.
- `tasklane doctor` checks an installation (database, migrations, git, data directory, plug-ins, Docker, public address) and says what to fix.
- `/metrics` (Prometheus format) for tickets, the queue, repositories and model usage; off unless `TASKLANE_METRICS_TOKEN` is set.
- Housekeeping removes sandbox containers a crashed daemon left running (only containers this engine named, older than the longest run).
- The developer agent can look at the app it builds: an optional preview command and port per repository start the app inside the Docker sandbox, and `tl-shot` takes a screenshot of a local address with Obscura (installed only when you pass its checksum) for the agent to read. Tested with a stand-in for Obscura; the real binary has not been run here.
- Every model run is recorded in the `runs` table (who, how long, tokens), and the ticket page shows the tokens it used.
- Admission control for CI: no new development starts while `TASKLANE_MAX_IN_CI` pull requests (default 5) wait for checks.
- The core refuses a plug-in that declares a `protocol_major` it does not know (as docs/design/05-plugin-protocol.md always said). A plug-in that does not declare one is still accepted.
- Test databases left by earlier runs are dropped automatically (their process is gone), so a long-lived test Postgres no longer fills up.
- The chat group a ticket was reported from (`/task`) is told how it ended or what it needs: merged, stopped, needs information, possible duplicate, closed. Each outcome is announced once.
- The orchestrator crate rebuilds when a migration file is added (a stale embedded schema could otherwise hide a new migration).
- Behaviour change: approving a ticket in a `triage` repository no longer switches the repository to `full`. `triage` now means what it says (every ticket asks); you switch a repository to `full` yourself.
- New `docs/autonomy.md` and a README section: when the agents act alone, when a person is needed, who may approve what, and every setting that turns the dial.
- Per-repository notes for the agents, written only by people in the console and added to every agent's instructions for that repository.
- `.tasklane.yml` in a repository: extra protected paths, a smaller diff limit, a mode cap and `always_ask`. It can only tighten; every other key is ignored.
- `TASKLANE_DEV_TIMEOUT_SECS` and `TASKLANE_CI_TIMEOUT_SECS` set the wall time of a development run and the wait for checks.
- Duplicate proposals: a new ticket whose title closely matches an earlier ticket of the same repository is held as `duplicate_proposed` before any model run. A person confirms (closed as a duplicate) or rejects (triaged as usual, never proposed again) in the console. The match is plain word overlap, so it is predictable; the model never decides this.
- GitHub webhooks verified with real deliveries (issue opened, comment created) through a temporary tunnel that forwarded only the webhook path.
- Agent sandbox network limit: `docker-compose.egress.yml` adds an internal Docker network and a small allowlist proxy (`docker/egress-proxy/proxy.py`) so the agent container can reach only the model API. Checked with the real CLI inside the sandbox.
- Merge queue: a repository that merges through GitHub's merge queue now fails the ticket with a clear message instead of an opaque refusal.
- Comments are marked by their author's relationship to the repository. A stranger's comment is tagged `trust="untrusted"` for every agent role and is left out of the developer's prompt entirely; the system rules tell the agent never to follow it.
- The GitHub webhook signature check is tested against the vector GitHub publishes in its documentation.
## 0.1.0-alpha.1 (2026-11-09)

First pre-release. See the status section of the README for what has and has not run for real.

- README rewritten in the shape of well-kept open-source projects: one-line pitch, how it works, quickstart, safety model, features, FAQ, an honest status section, and where to contribute.
- Live map: class names are drawn last and only where no chip or node is in the way, so they no longer sit half behind a label.
- The Tickets list is paged (25 per page) with the search kept across pages; a `%` or `_` in the search is taken literally.
- A prompt-injection test set runs against every agent role: crafted ticket, comment, diff and question text cannot close its block or change the tool list.
- Security fix, found in a real run: pressing Retry on a failed ticket could send a pull request straight to merge even though the reviewer had asked for changes. The reviewed head is now recorded, and a retry merges only a head a reviewer approved; anything else goes back through CI and review.
- The developer role may run `chmod +x` / `chmod -x` (the executable bit is part of a change); nothing else of chmod.
- Docker sandbox fix, found in a real run: the container ran as user 10001 and could not write the checkout, so the agent changed nothing. It now runs as the checkout's owner (never root). A "no change" failure now carries what the agent said, and the developer rules tell it to make every change it can.
- A repository GitHub blocks (403/451) is skipped during sync instead of stopping the sweep for every repository.
- Token budget per ticket (`TASKLANE_MAX_TOKENS_PER_TICKET`, default 3000000): every model run is counted, and a ticket over its budget goes to `failed` before another run starts.
- Tickets that nobody answers are closed as stale (14 days waiting for the reporter, 30 days waiting for an approval). A timer only closes, it never approves.
- A second tasklane install on the same repository stays out of the way: repositories are claimed through an atomic ref on GitHub (oldest claim wins), the other install skips them and shows who owns them, and a person can take a repository over.
- Console: live map in 3D (orbit, zoom, pan) with agents and repositories grouped on tiers; paged, searchable, filterable repository list; one colour meaning per button role (primary, approve, danger, warn, ghost); the agent can sign in with a Claude subscription token instead of an API key; new logo.

A pre-release for supervised use. It will be tagged when the owner says so.

Guided setup in the console: Connections page, one-click GitHub App creation (GitHub's manifest flow), agent key, isolation choice and a test button; a plain install needs only a database password, and the admin token is generated on first start. New console: live map with replay (agents glow while working), overview with live board, ticket page with stepper, timeline and colour diff, repositories with agent settings, people and roles, health and setup checklist, activity feed, light and dark, mobile layout, installable as an app.

Live dry run (a throwaway private repository, live GitHub App, live Claude, about 10 minutes of tokens): an issue went to a merged pull request unattended twice in about 70 seconds to 2 minutes; a request to change a workflow file stopped for approval of the plan and then of the exact diff, and GitHub itself refused the push without the Workflows permission, which tasklane reported clearly instead of retrying; the pause switch stopped all work; a vague issue produced one set of questions. The run found and fixed three bugs that fakes could not show: GitHub strips hidden markers from comments (own comments are now recognised by the bot's login), triage had no view of the repository (it now gets a read-only checkout and asks only when it cannot proceed), and tasklane's own plan comment tripped the risk rules.

Also in this release: a minimal sandbox image (`Dockerfile.sandbox`) and an entrypoint-independent sandbox command, a backlog cap for issues created directly on the source, and a readiness check that includes the plug-ins.

## 0.0.1

First pre-release. Triage, development, pull request, CI wait, review and merge run end to end in tests against a real Postgres and git, with fake GitHub, Claude and Telegram services. Not yet run against the live services.

Hardening from an independent review before the first tag: protected-path floor that cannot be sidestepped (non-ASCII names, renames, nested paths, empty lists), approvals bound to the exact diff and shown to the approver, text re-scored before development, git never runs agent-controlled config, tokens passed by environment, collaborator-only unattended work, console sessions on the server with sign-out and lockout, sandbox gate for the engine, failed tickets recoverable from the console, dead plug-ins restarted, stuck tickets failed by the reaper, retention.
