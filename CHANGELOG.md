# Changelog

## Unreleased

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
