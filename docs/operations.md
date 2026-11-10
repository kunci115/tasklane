# Running tasklane

tasklane is a single daemon plus Postgres. It reads tickets from a ticket source (GitHub in stage 1), has an AI agent triage and fix them, opens pull requests, and merges to a development branch after CI and review. **People approve anything risky; the system never touches production.**

## What you need

- Docker with Compose (or a Linux host with Postgres 14+, git, python3, openssl, node and the Claude Code CLI).
- A GitHub App installed on your organization or account.
- An Anthropic API key (or a Claude Code OAuth token) for the agent.
- A TLS reverse proxy in front of port 8080 if the console or the webhook is reachable from outside.

## Using GitLab instead of GitHub (experimental)

`plugins/source-gitlab` is a ticket source for GitLab (gitlab.com or your own server): issues are tickets, merge requests are pull requests, checks come from the merge request's pipeline, and an author is trusted when they have the Developer role or higher. It supports everything the GitHub source does (webhooks, the repository rules file, feedback from people on the merge request, and the two-installs ownership claim, done with a branch named `tasklane-owner-<time>-<install>`).

```bash
export TASKLANE_SOURCE_CMD="python3 -I /opt/tasklane/plugins/source-gitlab/source_gitlab.py"
export TASKLANE_SOURCE_ENV=GITLAB_URL,GITLAB_TOKEN,GITLAB_WEBHOOK_SECRET,GITLAB_GROUP
export TASKLANE_SOURCE_NAME=gitlab
export GITLAB_URL=https://gitlab.com                  # or your server
export GITLAB_TOKEN=...                                # a dedicated bot's access token: scope api, role Developer
export GITLAB_WEBHOOK_SECRET=...                       # the secret token you set on the webhook
export GITLAB_GROUP=your-group                         # optional: only projects under this group
```

Add a webhook on the group or project that points to `https://<your host>/api/v1/webhook/source`, with that secret token and the triggers Issues events, Comments, Merge request events and Pipeline events. Run `tasklane doctor` to check the plug-in answers.

Differences you should know about: **GitLab cannot mint a token that lives only minutes**, so the per-run token tasklane hands to git is the bot's own token. Keep that bot's reach small (one group, Developer role) and rotate its token. The GitHub-App one-click connection in the console does not apply; the settings above are the setup. Links from the console to issues and merge requests are not shown for GitLab yet. **Verified here:** the plug-in against a fake GitLab (every operation, trust, webhooks, ownership, error handling). **Not verified here:** a real GitLab, so do a supervised dry run on a throwaway project first.

## Who gets the credit in the GitHub history

The agent works on behalf of a person, so the person it answers to gets the credit. On the **People** page each person has a GitHub login and a **credit me** box (off by default; it is a claim on their public contribution graph, so they choose). For a change:

- the **author** of the commit is the last person who approved it and turned credit on; if nobody had to approve, the person who opened the issue (if they turned it on). `tasklane` stays the **committer**, so the history shows both. GitHub counts the commit in the author's contribution graph through their `ID+login@users.noreply.github.com` address, which tasklane looks up once and remembers.
- other people who turned credit on and took part (the requester, an earlier approver) are `Co-authored-by`.
- trailers `Requested-by` and `Approved-by` name who asked and who approved, whatever anyone chose, and the pull request says the same in one line (as plain names, no `@mention`).

Notes: GitHub counts commits only on a repository's default branch, so a repository that merges into `dev` shows the squares once `dev` reaches the default branch. The author is fixed when the agent commits, so a person who approves only the final diff (after the commit) is named in the pull request line and trailers of later commits, not as author of that commit. Only logins that fit GitHub's rules ever reach a commit message. `TASKLANE_ATTRIBUTION=off` turns all of it off.

## Several model accounts (when one runs out of usage)

The **Model accounts** page in the console keeps a list of accounts for the Claude engine: a subscription token (from `claude setup-token`) or an Anthropic API key each. The first account that is ready does the work. When it reports that its usage limit is reached it sleeps for 30 minutes (`TASKLANE_CLAUDE_COOLDOWN_SECS`), a refused credential sleeps a day, and the same run goes on with the next account. If every account sleeps, tickets wait for the first to wake up; waiting costs no attempt, so a limit that lasts hours does not fail tickets.

What carries over is the ticket, its feedback and the working copy, and the developer is told that part of the work may already be there. A conversation cannot move between accounts, so the new run reads the code again. Tokens are stored in the data directory (`secrets/claude-accounts.json`, readable only by tasklane), shown only by their last four characters, and never written to the audit log. A subscription token is for one person's own use; for a team, use API keys. Without the Docker image, set `TASKLANE_CLAUDE_POOL_FILE` yourself. Details: [plugins/engine-claude/README.md](../plugins/engine-claude/README.md).

## Using another coding agent (Codex, Gemini CLI, Aider, ...)

Claude is built in, but `plugins/engine-cli` runs **any command-line coding agent** as the agent engine. It keeps the same rules as the Claude engine (the Docker sandbox is required, the read-only roles get a read-only mount, the egress allowlist applies, secrets are passed by name and scrubbed from output, and the agent's answers are checked before tasklane uses them). You supply the command and an image that contains the agent:

```bash
# 1. an image with your agent (the sandbox image is the base)
cat > Dockerfile.myagent <<'EOD'
FROM tasklane-sandbox
USER root
RUN npm install -g @openai/codex        # or the agent you use
USER agent
EOD
docker build -f Dockerfile.myagent -t tasklane-codex .

# 2. start tasklane with the generic engine
export TASKLANE_ENGINE_CMD="python3 -I /opt/tasklane/plugins/engine-cli/engine_cli.py"
export TASKLANE_ENGINE_ENV=TASKLANE_CLI_COMMAND,TASKLANE_CLI_COMMAND_READONLY,TASKLANE_CLI_NAME,TASKLANE_CLI_CREDENTIALS,OPENAI_API_KEY,TASKLANE_SANDBOX,TASKLANE_SANDBOX_IMAGE,TASKLANE_SANDBOX_NETWORK,TASKLANE_SANDBOX_PROXY
export TASKLANE_CLI_NAME="Codex"
export TASKLANE_CLI_COMMAND="codex exec --full-auto -"          # the prompt arrives on stdin
export TASKLANE_CLI_CREDENTIALS=OPENAI_API_KEY                   # passed to the container by name
export TASKLANE_SANDBOX=docker TASKLANE_SANDBOX_IMAGE=tasklane-codex TASKLANE_SANDBOX_NETWORK=tasklane-agent
export EGRESS_ALLOW=api.openai.com                               # the model API this agent talks to
```

- `{prompt}` as one whole argument passes the prompt as an argument instead of on stdin (for agents that take it that way).
- `TASKLANE_CLI_COMMAND_READONLY` is an optional command for triage, review and questions, for example with the agent's own read-only flag.
- Run `tasklane doctor`, then the console's agent test, before real work.

What you give up compared with the Claude engine: tasklane cannot restrict the agent's own tool list (the read-only roles are protected by the read-only mount in the Docker sandbox, which is enforced), and the agent's token use is not reported, so the per-ticket token budget does not apply. The example commands above are the agents' documented non-interactive modes; flags change between versions, so check your agent's documentation. **Verified here:** the plug-in's behaviour with a stand-in agent (prompt on stdin and as an argument, JSON and edit roles, credentials by name, scrubbing, timeouts, the Docker command). **Not verified here:** a real Codex, Gemini or Aider run.

## Letting the agent see what it builds (Obscura)

The developer agent can look at the app it is changing, using [Obscura](https://github.com/h4ckf0r0day/obscura), a small headless browser written in Rust (Apache-2.0). It is **off by default** and works only in the Docker sandbox:

1. **Build the sandbox image with Obscura.** Download the release archive yourself, check it, and give the build its SHA-256 (the build fails without it, so nothing unreviewed is installed):

   ```bash
   curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-linux.tar.gz
   sha256sum obscura-x86_64-linux.tar.gz
   docker build -f Dockerfile.sandbox -t tasklane-sandbox \
     --build-arg INSTALL_OBSCURA=1 --build-arg OBSCURA_SHA256=<that checksum> .
   ```
2. **Tell tasklane how to start the app.** On **Repositories**, open a repository's settings and fill **Preview: command** (for example `python3 -m http.server 3000`) and **Preview: port**. You set these, never the repository, because the command runs inside the sandbox.
3. The preview only starts in a sandbox on the egress-limited network (`docker-compose.egress.yml`, which sets `TASKLANE_SANDBOX_PROXY`); without it the developer run is refused, because a page the agent builds could otherwise fetch other addresses. The sandbox starts that command in the background before the agent, waits for the port, and lets the agent run exactly one extra command, `tl-shot http://127.0.0.1:<port>/<path> <name>`. It writes a PNG that the agent opens with its Read tool. `tl-shot` accepts only `http://127.0.0.1`, `http://localhost` and `http://[::1]` addresses, so it cannot be used to reach anything else, and the sandbox has no route to the internet anyway.

Limits to know about: the app must start inside the sandbox, which has no internet (unless you widened `EGRESS_ALLOW`), so it works for apps that are static or already carry their dependencies; Obscura draws pages with its own engine, and its documentation warns that some CSS, media and font rendering can differ from Chrome; the prebuilt Obscura release must include the rendering feature for screenshots. **Verified here:** the settings, the argument handling, the address checks and the start-up order, all tested with a stand-in for Obscura. **Not verified here:** the real Obscura binary (it was not run in this environment), so do one check before relying on it: `docker run --rm --entrypoint sh tasklane-sandbox -c "node -e \"require('http').createServer((q,r)=>r.end('<h1>hello</h1>')).listen(3000)\" & sleep 2; tl-shot http://127.0.0.1:3000/ check && ls -l /tmp/shots"` (the sandbox image has Node, so a one-line Node server stands in for your app).

## Checking an installation

`tasklane doctor` checks an installation without starting it: the database and migrations, `git`, the data directory, that the ticket source and agent engine plug-ins start and answer, Docker when the Docker sandbox is chosen, and the public address. Each line says what is wrong and what to do; it exits with an error if something that stops tasklane from working fails. With Docker Compose: `docker compose -f docker-compose.prod.yml run --rm tasklane tasklane doctor`.

## Rules a repository can set for itself

A repository may carry a `.tasklane.yml` on its default branch. It can only make tasklane **stricter** in that repository, so a contributor cannot loosen anything by editing it:

```yaml
protected_paths:        # extra paths that always need approval of the exact diff
  - payments/
  - config/prod.yml
max_diff_lines: 200     # a smaller diff limit than the install's (never a larger one)
mode_cap: triage        # the most this repository may be set to: watch, triage or full
always_ask: true        # every ticket waits for a person's approval
```

A path is a directory (`payments/`, matched anywhere in the tree), a file name or a name prefix; `*` patterns are not supported and are ignored. Other keys (a test command, auto-merge, instructions for the agent) are ignored on purpose: they would let repository content steer or loosen the pipeline. The file is read before each triage and each development run; if it cannot be read, the last known rules stay. Instructions for the agent and recorded notes are set in the console (**Repositories**), not in the repository.

## Notes for the agents

On **Repositories**, open a repository's settings to record short notes ("the staging branch is develop", "never touch `generated/`"). Every agent working on that repository is given them with its instructions. Only people write notes: nothing a model or a ticket says is ever stored there. Up to 20 notes of 1000 characters are used, newest first.

## Duplicates

When a new ticket's title closely matches an earlier ticket of the same repository (the same significant words, not a similar topic), it is held as **Possible duplicate** before any model run. A person answers on the ticket page: close it as a duplicate, or handle it as a new ticket (it is then never proposed again). The model never decides this. With `TASKLANE_DUPLICATE_MODEL=1` the model may also *suggest* a duplicate among the 30 most recent tickets; it can only name one of those, and a person still answers.

## Two installs on one repository

Two tasklane installs on the same repository would both triage every issue, comment twice and open duplicate pull requests. tasklane prevents this without any extra server:

- Each install has an identity (a short name from the machine's host name, set `TASKLANE_INSTANCE_NAME` to choose one, and a random id). It is shown on the **Repositories** page.
- The first time an issue arrives in a repository that is in `triage` or `full`, the install **claims** the repository on GitHub by creating the ref `refs/tasklane/owner/<time>-<name>_<id>`. Creating a ref is atomic, the **oldest claim wins**, and every install computes the same winner, so two installs that start at the same moment settle without talking to each other.
- An install that does not own a repository skips its issues, holds any queued work for it and shows **Handled by another tasklane install** on the repository card. Nothing is commented or pushed.
- A result is trusted for a few minutes (ten when you own the repository, five when you do not), so a quiet repository costs no API calls and an active one costs about one call every few minutes.
- Putting a repository in `watch` gives the claim back. If the other install is gone for good, press **Take over** on the card: it removes the other claim and takes the repository. That is a decision for a person, never automatic, because GitHub gives no safe way to know whether the other install is only slow.
- A source plug-in without this guard is not blocked; an empty repository (no commit to point a ref at) cannot be guarded yet.

One install per organisation is the supported setup. Several people use the same console with their own roles, and more throughput comes from more workers on the same database, which the queue already handles (leases and fencing tokens).

## Quick start (Docker)

```bash
git clone https://github.com/kunci115/tasklane && cd tasklane
export POSTGRES_PASSWORD=$(openssl rand -hex 16)
docker compose -f docker-compose.prod.yml up -d --build
docker compose -f docker-compose.prod.yml logs tasklane      # prints the admin token once
```

Open `http://localhost:8080`, sign in with that token, and follow the **Getting started** card:

1. **Connect GitHub.** Press the button. GitHub opens with the App already filled in; press **Create GitHub App**, then **Install** and choose only the repositories tasklane should work on. tasklane stores the App's credentials itself. You never create a key or edit a config file.
2. **Connect the agent.** Choose how it signs in: an API key, or your Claude subscription (run `claude setup-token` on a computer where you are signed in to Claude Code and paste the token; the work counts against your plan limits). Then choose how the agent is isolated and press **Run the test**.
3. **Choose repositories.** New ones start in triage: tasklane plans, and every ticket waits for your approval before any code is written.
4. Open an issue on one of them. It shows up on the Overview within a minute.

Where it listens: `127.0.0.1:8080`. For anything but a local try, put a TLS reverse proxy in front and set `TASKLANE_PUBLIC_URL` to its address; with an `https://` address GitHub can also send webhooks, so tickets are picked up at once. Without that, tasklane checks for new issues every minute.

The secrets you save in the console (the GitHub App's key, the model key) are files in the `workdata` volume, readable only by tasklane. The admin token is in the same place. Back that volume up like any other secret.

## The console at a glance

Sign in with `TASKLANE_ADMIN_TOKEN`. The left menu has:

- **Overview**: a number of things waiting for you, a list of what needs a decision (with a Review button), and a live board of every ticket by stage. A "Getting started" card guides a new install through connecting a source, making the agent safe, choosing repositories and getting a first ticket.
- **Live map**: repositories in a ring around tasklane, tickets as coloured dots, and the agents (triage, developer, reviewer, CI, PR and merge, and you) as nodes that glow while they work, with a line from each ticket to the agent that has it. Tickets waiting for you gather at the "You" node, so a pile-up is visible at a glance. **Replay** plays back the last hour to 7 days at up to 1200x with a time slider and skips idle time. It is for seeing what the system is doing now, finding where tickets pile up, and understanding what happened afterwards. Replay data holds only the event type and a few state fields, never ticket text.
- **Tickets**: a searchable list with views (needs you, in progress, done, failed). Opening one shows a progress stepper, a timeline in plain sentences, the plan, and the full colour diff of the change. **A decision box appears when a person is needed**: it says whether you are approving a plan or an exact change, why a person is needed, and offers Approve and Reject. A stopped ticket offers Try again and Close.
- **Repositories**: a mode switch per repository (watch, triage, full), its protected paths, and **Agent settings** (instructions for the agent and a test command it may run).
- **People**: who may approve and at which level.
- **Chat groups**: register Telegram groups.
- **Activity**: everything tasklane and people did.
- **Health & setup**: whether each part is connected, whether the agent is sandboxed, the queue, and recent failures.
- **Pause all work** (bottom left) stops all new work at once. Light or dark and sign out are next to it.

Pages refresh by themselves every few seconds while the tab is open. Without JavaScript the pages still work; they just do not refresh on their own. To use it as an app, open it in Chrome or Safari and choose "Install" or "Add to Dock".

## 1. Create the GitHub App by hand (alternative to the one-click setup)

GitHub: Settings, Developer settings, GitHub Apps, New. Then:

- Webhook URL: `https://<your host>/api/v1/webhook/source`, with a random webhook secret (`GITHUB_WEBHOOK_SECRET`).
- Repository permissions: Issues (read and write), Pull requests (read and write), Contents (read and write), Metadata (read), Checks (read), Commit statuses (read). Environments/Administration (read) is optional.
- Subscribe to: Issues, Issue comment, Pull request, Check suite.
- Generate a private key (PEM) and note the App id; install the App on your organization or account and note the installation id (the number in the installation URL).

**Workflows permission is left out on purpose.** GitHub refuses any push that creates or changes files under `.github/workflows/` unless the App has the Workflows permission. With it left out, a ticket that needs a CI change stops with a clear reason and a person makes that change by hand. Grant Workflows (read and write) only if you want tasklane to edit CI configuration, and then keep an approval on those paths (they are protected paths, so every such change already needs approval of its exact diff).

The App never holds write access in the agent's hands: the daemon asks GitHub for a separate token limited to one repository and valid for about an hour each time it pushes or merges.

## 2. Configuration by environment variables (advanced)

The console covers everything a normal install needs. Environment variables are for automated setups, and they win over what the console saved. The daemon refuses to start with a clear message when a required value is missing or weak.

| Variable | Required | Meaning |
|---|---|---|
| `TASKLANE_DATABASE_URL` | yes | Postgres URL. Migrations run on start. |
| `TASKLANE_ADMIN_TOKEN` | no | At least 24 characters. Signs in to the console. If unset, one is generated on first start and kept in the data volume. |
| `TASKLANE_SOURCE_CMD` | no | Command that starts the ticket-source plug-in. Defaults to the shipped GitHub plug-in. |
| `TASKLANE_ENGINE_CMD` | no | Command that starts the agent-engine plug-in. Defaults to the shipped Claude plug-in. |
| `TASKLANE_SOURCE_ENV` / `TASKLANE_ENGINE_ENV` | no | Comma-separated names of variables to pass to that plug-in. **Nothing else is inherited**, apart from `PATH`. |
| `TASKLANE_CHAT_CMD`, `TASKLANE_CHAT_ENV` | no | Chat plug-in (see "Chat"). |
| `TASKLANE_BIND` | no | Listen address, default `127.0.0.1:8080` (`0.0.0.0:8080` in the image). |
| `TASKLANE_PUBLIC_URL` | no | If it starts with `https://`, the session cookie is marked `Secure`. |
| `TASKLANE_DATA_DIR` | no | Git mirrors and worktrees, default `./data` (`/data` in the image). Keep it on a volume. |
| `TASKLANE_JWT_SECRETS` | no | `name:secret,name:secret` (secrets of 16+ characters), one per app that embeds the report button. Without it the report endpoint refuses everyone. |
| `TASKLANE_WORKERS` | no | Parallel workers, default 2. |
| `TASKLANE_MAX_TOKENS_PER_TICKET` | no | Model tokens (in and out, cache reads included) one ticket may use across all its runs, default 3000000. Past it the ticket goes to `failed` with that reason and a person decides. |
| `TASKLANE_DEV_TIMEOUT_SECS` | no | Wall time one development run may take, default 1800 (limits 60 to 86400). A run that exceeds it is stopped and counts as an attempt. |
| `TASKLANE_CI_TIMEOUT_SECS` | no | How long a pull request's checks may stay pending before the ticket fails, default 3600. |
| `TASKLANE_MAX_IN_CI` | no | No new development starts while this many pull requests are waiting for CI, default 5. |
| `TASKLANE_METRICS_TOKEN` | no | Turns on `/metrics` (Prometheus text format: tickets by state, queue jobs by status, repositories by mode, model tokens and runs, and outcomes: tickets merged, merged on the first try, failed, reviewer verdicts, red CI runs, tokens per role). Needs 16 characters or more; a scraper sends it as `Authorization: Bearer <token>`. Without it the endpoint does not exist. |
| `TASKLANE_DUPLICATE_MODEL` | no | `1` also asks the model whether a new ticket repeats one of the 30 most recent tickets of the repository, when the word match found nothing. It can only name a listed ticket and a person still decides. Costs a small model run per new ticket; off by default. |
| `TASKLANE_MAX_DEV` | no | Development runs at once, default 1. Raise it only after you measure your CI capacity. |
| `TASKLANE_USER_DAILY_LIMIT` | no | Reports per reporter per day, default 10. |
| `TASKLANE_SYNC_SECS` | no | Polling interval for the sync backstop, default 60, minimum 5. |
| `TASKLANE_SOURCE_NAME` | no | Registry name of the source, default `github`. |
| `TASKLANE_RETENTION_DAYS` | no | Audit rows older than this are deleted (default 365, minimum 7). Finished jobs are deleted after 30 days. |
| `TASKLANE_ALLOW_NO_CHECKS` | no | `1` treats a pull request with no CI checks as green. Default off. |
| `TASKLANE_ATTRIBUTION` | no | `off` stops naming who requested and approved a change (commit trailers, pull request line, credit). Default on. See "Who gets the credit". |

The agent-engine plug-in has its own switch, `TASKLANE_ALLOW_UNSANDBOXED=1`, explained in section 7. Plug-in specific variables are documented in `plugins/source-github/README.md` and `plugins/engine-claude/README.md`.

## 3. How a repository becomes active

1. The sync job lists every repository the App can see. New ones start in **triage** mode (archived ones in **watch**).
2. New issues in a **triage** repository are read and planned by the agent. Nothing is changed until a person approves a ticket; the **first approval turns the repository to full mode** automatically.
3. In **full** mode, a ticket that the rules and the agent both rate level 1 runs through to a merge without a person, **if its author is a collaborator** (owner, member or collaborator of the repository, or tasklane itself acting for a reporter). An issue from anyone else always waits for approval. Levels 2 and 3 wait for approval first.
4. Change a repository's mode at any time in the console, under "Repositories". **watch** stops all activity for it.

**Which branch, and when the issue closes.** Pull requests go to the repository's default branch unless you set "Branch pull requests go to" (for example `dev`) in its Settings. After the merge the ticket is `merged`. The issue is closed (with a note) only when the merged commit is part of the default branch, so a repository that requires the change to reach `main` first keeps the issue open until then. The check runs on each sync pass.

Issues that existed before you installed the App are not imported on their own (the first poll only sets a baseline, so the agent does not comment on years of old issues). To bring them in, press **Import open issues** on the repository's card under "Repositories"; each open issue becomes a ticket. Or create a new issue, or use the report button.

## 4. Approving, pausing, failing

- A ticket that stops with an error gets a fixed comment on the issue ("needs a person, the details are in the console"); the reason (git output, the agent's words) is shown only in the console, because the issue may be public.
- A ticket that needs a person shows as **waiting for approval** in the console and carries a comment on the issue. The ticket page shows the plan and, once there is one, the full diff. The approval is bound to a hash of exactly that text; if it changes, the old approval is refused.
- **Approvals are asked for twice when needed.** A plan can be approved, and then the developed change still stops for approval of the diff itself if it touches protected paths (CI config, Dockerfiles, migrations, secrets, infrastructure, and a repository's own list) or is large. The ticket text is also scored again just before development, so a comment added after approval cannot smuggle in a riskier job.
- The protected-path rules always apply, even for a repository whose own list is empty; a repository's list only adds to them (`crates/tasklane-core/src/paths.rs`). Moving a protected file away counts as touching it.
- Levels: 1 runs by itself, 2 needs a `team_approver` or `owner`, 3 needs an `owner`. The console admin is an owner. Add people in the `principals` table (`channel`, `ref`, `role`).
- **Pause everything** in the console stops all job pickup instantly; running jobs finish their current step. Resume restarts.
- A ticket that cannot continue (CI red twice, the agent changed nothing three times, a branch that moved under review, a worker that kept dying) goes to **failed**. The public issue only says that a person is needed; the reason is in the console, where **Try again** and **Close it** are on the ticket page. Nothing retries forever.

## 5. The report button (for your own apps)

Your app's server signs a short token and calls the API; the browser never sees the secret.

```python
import hmac, hashlib, base64, json, time, urllib.request

def b64(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
def sign(claims, secret):
    h = b64(b'{"alg":"HS256","typ":"JWT"}'); p = b64(json.dumps(claims).encode())
    s = b64(hmac.new(secret.encode(), f"{h}.{p}".encode(), hashlib.sha256).digest())
    return f"{h}.{p}.{s}"

token = sign({"iss": "shop", "sub": "user-42", "svc": "your-org/your-repo", "exp": int(time.time()) + 300}, "the-shared-secret-for-shop")
req = urllib.request.Request("https://tasklane.example.com/api/v1/report",
    data=json.dumps({"title": "Printing fails", "body": "Steps..."}).encode(),
    headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
print(urllib.request.urlopen(req).read())
```

`iss` is the name from `TASKLANE_JWT_SECRETS`, `sub` identifies the reporter (limits are per reporter), and **`svc` is the repository the report is for. Your server sets it, so a user cannot aim a report at another repository.** Responses: `201` created, `200` duplicate (same title from the same reporter in a day), `401`, `404`, `409` (repository in watch mode), `422`, `429`.

## 6. Chat (Telegram)

Set `TASKLANE_CHAT_CMD="python3 -I /opt/tasklane/plugins/chat-telegram/chat_telegram.py"` and `TASKLANE_CHAT_ENV=TELEGRAM_BOT_TOKEN`, and put the bot token in the environment. Create the bot with BotFather and turn **privacy mode off** (or make it an admin) so it sees commands in groups; see `plugins/chat-telegram/README.md`.

1. Add the bot to a group. It replies once with the group id (and nothing else) because the group is not registered.
2. In the console, **Chat groups**, register that id for a repository and set a daily question limit.
3. In that group, **anyone** can use `/ask <question>` (answered by a read-only agent from the repository's code, capped per group per day) and `/task <title>` (opens a normal ticket; it still goes through triage and approval).
4. When a ticket needs approval the bot posts it with **Approve / Reject buttons**. A press counts only if the person is in the `principals` table with `channel = 'telegram'`, `ref = <their Telegram user id>` and a role high enough for the level. Typed replies never approve. The button is bound to a hash of the plan, so a stale button is refused.
5. The group a ticket was reported from with `/task` is also told how it ends: merged, stopped (with the reason), needs information, possible duplicate, or closed. Each outcome is announced once, and tickets made some other way are not announced in chat.

Anyone in a registered group can read what `/ask` can read from that repository. Keep groups private and watch who joins.

## 7. Security model and honest limits

- Agents hold no tokens, never push, never merge. The daemon pushes and merges with per-run tokens, passed to git through its environment, not on a command line. Git never runs inside the agent's directory in the ordinary way: it is pointed at a trusted git directory with every program-running config key switched off.
- Risk levels come from fixed rules on the ticket text and the real diff; the model can raise a level, never lower it. The rules are starting defaults in `crates/tasklane-core/src/floor.rs` and `paths.rs`, deliberately broad.
- Ticket text, comments, diffs and chat messages are untrusted input.
- **The agent engine refuses to run without a sandbox.** Every role runs model-driven file tools on your server. Without isolation they run as the daemon's user and can read its environment, its database URL and its keys (for example through `/proc`), and a question in a chat group could return them. Either:
  - choose **Docker sandbox** in Connections > Connect the agent. The agent then runs in a locked-down container (read-only root, no capabilities, process and memory limits, only the checkout mounted) built from `Dockerfile.sandbox`, which holds only the agent program and none of your secrets. It needs the Docker socket available to tasklane: start with `docker compose -f docker-compose.prod.yml -f docker-compose.sandbox.yml up -d --build` after `export DOCKER_GID=$(stat -c %g /var/run/docker.sock)`. **Mounting the socket is as powerful as root on that machine, so use a machine you dedicate to tasklane.** For the container's network, the built-in `bridge` reaches everything. To limit the agent to the model API, add the third file: `docker compose -f docker-compose.prod.yml -f docker-compose.sandbox.yml -f docker-compose.egress.yml up -d --build`, then choose the network `tasklane-agent` in the console. That network has no route to the internet; the only way out is `docker/egress-proxy/proxy.py` (Python standard library, about 90 lines), which allows HTTPS `CONNECT` to the hosts in `EGRESS_ALLOW` (default `api.anthropic.com`) and refuses every other host, port and any address that is private or loopback. Add a host to `EGRESS_ALLOW` only if you must. **Verified:** with that network and proxy, the real CLI answered from inside the sandbox using a Claude subscription token; `example.com` and `github.com` were refused and a direct connection failed. **Verified:** the container starts with all those restrictions, runs the real CLI, and is killed on timeout; the image contains the Docker client. **Not verified:** a full answer from inside the sandbox with a valid key, started through the socket from the tasklane container; or
  - set `TASKLANE_ALLOW_UNSANDBOXED=1`, only on a host that holds nothing else, with repositories and reporters you fully trust.
  Answers returned to chat have known token shapes and configured keys removed, which is a safety net, not a boundary.
- Anyone who can open an issue on a repository tasklane watches can cause **triage** (a read-only model run, so cost). Their tickets never run unattended. At most 25 triage runs wait at once; further issues are held back and picked up as the queue drains, so a flood cannot queue unlimited model runs. Prefer private repositories or restricted issue creation on the source.
- **What has run for real:** the first-run flow in the real container (start with only a database password, admin token printed once, sign in, the Connections pages, the GitHub App manifest generated with the right permissions); and
   one supervised dry run against live GitHub and live Claude on a throwaway private repository: unattended issue-to-merge twice, a protected-path change that stopped for approval of its exact diff, the pause switch, the question flow, and GitHub's refusal of a workflow push. That run found three bugs, all fixed. A second supervised run used the one-click App connection, a Claude subscription token and the Docker sandbox (on Docker's default `bridge` network) against a private sandbox repository: issue to merged pull request, an executable file included, with the reviewer catching a missing executable bit. It found four more bugs, all fixed (the sandbox user could not write the checkout, Retry could merge a pull request the reviewer had not approved, one blocked repository stopped the whole sync, the developer could not set the executable bit). A later check with real Claude in the Docker sandbox showed the developer acting on reported feedback (a reviewer's complaint about a file mode) after a bug had kept feedback from it. Two installs on one repository were run side by side against real GitHub: the second skipped the issue and the first handled it. GitHub webhooks were also run for real, through a temporary tunnel that forwarded only the webhook path: a new issue and a new comment each reached tasklane as a signed delivery from GitHub and the ticket appeared at once. (The webhook was set on the repository, because the App created by the one-click flow has no webhook or events when tasklane has no public address; start tasklane with `TASKLANE_PUBLIC_URL` set to an `https://` address before pressing Connect GitHub and the App is created with the webhook and its events.) **What has not run for real:** Telegram (tested against a fake API only), and anything with more than one approver. Do your own supervised dry run on a throwaway repository first.
- The console signs in with one admin token and uses server-side sessions (8 hours, random id, per-session form token, sign-out, lockout after five wrong tokens). It has no user accounts: everyone who has the token is the owner.
- Logs go to stderr and contain job and ticket ids, not secrets. Metrics are off until `TASKLANE_METRICS_TOKEN` is set. A plug-in that crashes is started again on the next call; `/readyz` checks the database and that both plug-ins answer.

## 8. Backup, upgrade, troubleshooting

- **Backup:** `pg_dump` the database. The `/data` volume only holds re-creatable git mirrors.
- **Upgrade:** pull, rebuild, restart. Migrations are applied on start and never destructive.
- **Stuck job?** The reaper returns jobs whose worker died; a ticket that waits too long shows in the console. A paused system shows "paused" on the home page.
- **`readyz` fails:** the database is unreachable.
- **Nothing happens after installing the App:** check the repository's mode, that the issue is new (not older than the install), and the daemon log for `sync:` lines.

## People and their model accounts

Under **People** an owner can create a console sign-in for each person (a token shown once). They sign in with it, see the work, approve what their role allows (their name is recorded as the approver) and add their own Claude account under **My account**. Work that a person approved runs on their account first, then on the shared accounts if theirs is used up; nobody's account is used for somebody else's work. Owners keep the settings (Repositories, People, Model accounts, Connections). The admin token still exists and acts as the console admin.

Not included (planned for a managed edition): single sign-on, groups and finer roles than owner, team approver and member.

## Telling the agents what the team knows

The **Knowledge** page holds short notes that every agent is given (for example "answer in Indonesian"). Notes about one repository, the documents the agents should read first (paths such as `CONTRIBUTING.md` or `docs/architecture.md`) and the agent instructions are edited on that repository's card under Repositories; the Knowledge page lists them all. Only people write these; nothing a model or a ticket says is stored there. Do not put secrets in them: an agent can repeat what it is told. Uploads and an external wiki are not supported yet.

## Starting a new project

Press **Plan it with the AI first** instead of creating straight away: the AI asks what it needs, then proposes a list of tickets (the first sets up the skeleton and the test command). Edit or untick them, and confirm to create the repository and those tickets. Each ticket is then triaged and waits for your approval like any other.

**New project** in the console creates a repository, registers it in triage mode and opens a first ticket from your brief, so a project can start from an idea instead of an existing repository.

GitHub only lets an App create repositories in an organization, and only with the **Administration** permission (write). tasklane does not ask for that when it creates the App, to keep the App's rights small. To use this page: open the App's settings on GitHub, under Permissions add Repository permissions, Administration, Read and write, and accept the change in the organization. Install the App with **All repositories** so the new repository is included by itself; with selected repositories the new one has to be added to the installation by hand. The seeded `.tasklane.yml` keeps `.github/` (workflows) behind a person, because the App cannot change workflow files anyway.
