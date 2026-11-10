# Changelog

## Unreleased

- **Branch pull requests go to** (per repository, under Settings): for teams that merge into `dev` first. A merged ticket no longer leaves its issue open forever: tasklane closes the issue itself, with a note, once the merged commit is in the default branch (checked on every sync pass; a change merged straight into the default branch closes at the next pass). Until then the issue stays open and the ticket stays `merged`. New optional source operations `in_branch` and `close_item`. Squash-merging `dev` into `main` hides the original commit from the check; close those issues by hand.
- **Import open issues** button on each repository card: issues that were open before GitHub was connected were never picked up (the first poll only sets a baseline). The button turns them into tickets on request, oldest first, up to 500, and holds back what would exceed the triage queue limit. New optional source operation `list_open_issues`.
- Disconnecting GitHub in the console now archives the repositories that came through it (their tickets and history stay) and resets the sync cursor, so nothing lingers on the Repositories page; connecting again brings them back.
- Pull requests: when the AI approved a pull request but there is no Merge button, the row says why (default branch, repository not in full mode, or merging from here not switched on) and links to the switch, which is now open by default.

- **New project** page: start from nothing. tasklane creates a repository in an organization the GitHub App is installed on (private by default; README, a `.gitignore` for the template, and a `.tasklane.yml` that keeps `.github/` behind a person), registers it in triage mode and turns your brief into the first ticket, whose plan then waits for approval. It needs the App's **Administration** permission (write), which tasklane does not request by default; the error says what to change. New optional source operation `create_repo`.

- **Merge** button on the Pull requests page: it appears only for a pull request the AI approved at its latest commit, in a repository that is in full mode and switched on under "Merging from here" (off by default), and never when the pull request goes into the default branch. The orchestrator checks all of it again and GitHub merges only if the head is still that commit and its checks pass. Every press is in the audit log.
- The AI review now includes a Big-O analysis of the changed code (new optional `complexity` in the reviewer reply), posted in the comment and shown in the list; a new path worse than linear on growing data is a reason to ask for changes.
- The live map shows open pull requests as diamonds tied to their repository, to their ticket when tasklane opened them, and, while the AI reads one, to the review agent (the agent lights up and "Working now" lists it). Reviews and merges pulse. Click a diamond to open it on GitHub. The map reads the Pull requests list from cache and never asks GitHub itself.

- Fix: a long sync (every repository scanned after a reconnect) made pages that ask GitHub, such as Pull requests, wait behind it and time out. The GitHub plug-in now answers requests side by side (answers can arrive in any order; they carry the request `id`) and mints its installation token once.

- Fix: the AI review compared a pull request with the repository's default branch instead of the branch the pull request goes into, so work already on that branch (for example `dev`) counted as part of the pull request. It now asks the source for the target branch (new optional operation `get_pull`) and refuses closed pull requests. The review is also readable in the list row, with what to do next.

- **Review with AI** button on the Pull requests page: a read-only reviewer reads the pull request's diff (forks included) and posts its opinion as a comment on the pull request; the list shows running and last verdict. It never approves, merges or pushes, and it refuses repositories in watch mode.

- New **Pull requests** page: the open pull requests in every watched repository, whoever opened them (draft, author, branch, who is tasklane). Read only; opens at once and fills in, refreshed at most once a minute. New optional source operation `list_pulls`.

- Connect GitHub: the organization box accepts a pasted link (`https://github.com/Acme`) or `@Acme`, a name typed there means an organization even if the radio was left on personal, and the error messages say which field is wrong.

- Tickets of an archived repository no longer show in the Tickets list, the Overview board or the needs-you badge; a direct link to a ticket still opens its history.

- A repository the source no longer lists (you switched GitHub account or App, or access was removed) is archived on the next sync instead of staying on the board, and the Repositories page hides archived ones unless you press **Show archived**. They return by themselves when listed again.


- Credit in the GitHub history goes to the person the agent worked for. On the People page each person sets their GitHub login and a **credit me** box (off by default). The last person who approved a change (or whoever opened the issue, when nobody had to approve) becomes the commit's author with tasklane as committer, so it counts in their contribution graph; other opted-in people are `Co-authored-by`. Commits always carry `Requested-by` and `Approved-by` trailers and the pull request says who asked and who approved. Only valid GitHub logins reach a commit message; ticket titles can no longer add lines to it. New optional source operation `get_user`. `TASKLANE_ATTRIBUTION=off` switches it all off.

- Several model accounts: the console has a Model accounts page (add, order, disable, remove; tokens are never shown again). When an account reports its usage limit, the same run goes on with the next one; a limited account sleeps 30 minutes, a refused one a day. The developer is told that part of the work may already be in the working copy. When every account sleeps, jobs wait for the first to wake without using up an attempt (the plug-in protocol's `rate_limited` error now carries `retry_after_secs`). A conversation cannot move between accounts; the ticket, feedback and working copy carry over. Tested with a stand-in CLI.

- Engine role `device` (both engine plug-ins): given an incident, what was tried and the only allowed actions, the agent picks the next step (`continue`, `done` or `escalate`). Its input is marked data that cannot close its block, and its answer is checked. Used by the device worker of the commercial edition.
- Evaluations (`evals/`): 28 fixed cases for triage, reviewer, duplicate finder and device troubleshooter (a typo is level 1, a removed auth check under a typo ticket is never approved, text inside a diff or a log cannot steer the answer). `evals/run_evals.py` runs them through any engine plug-in, repeats runs to get a pass rate, and fails when a case passes less often than a saved baseline. CI checks the case files and the runner; running them against a model is yours to do after a prompt, model or engine change. See `docs/evals.md`.
- Outcome metrics on `/metrics`: tickets merged, merged on the first try (one attempt, no red CI, no requested changes), failed, development attempts spent on merged tickets, reviewer verdicts, red CI runs and model tokens per role. These are the numbers to watch when a prompt or model changes.
- When CI fails, the developer now gets what the failed checks reported (title, summary and text of a GitHub check run, the description of a status), not only their names. Chat still says only that CI failed.

## 0.1.0-alpha.2 (2026-11-10)

A large step since the first alpha: the developer now really receives feedback, pull request feedback from people, repository rules and notes, duplicate proposals, a second agent engine and a GitLab source (both experimental), an autonomy guide, and the fixes from an independent security review. Verified with real GitHub and Claude: the developer acting on feedback in the Docker sandbox, webhooks, two installs on one repository; see the README status section for what has not run for real.

- GitLab ticket source (`plugins/source-gitlab`, experimental): issues, merge requests, pipelines, webhooks, the repository rules file, merge-request feedback and the two-installs claim. Tested against a fake GitLab.
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
