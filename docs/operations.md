# Running tasklane

tasklane is a single daemon plus Postgres. It reads tickets from a ticket source (GitHub in stage 1), has an AI agent triage and fix them, opens pull requests, and merges to a development branch after CI and review. **People approve anything risky; the system never touches production.**

## What you need

- Docker with Compose (or a Linux host with Postgres 14+, git, python3, openssl, node and the Claude Code CLI).
- A GitHub App installed on your organization or account.
- An Anthropic API key (or a Claude Code OAuth token) for the agent.
- A TLS reverse proxy in front of port 8080 if the console or the webhook is reachable from outside.

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

When a new ticket's title closely matches an earlier ticket of the same repository (the same significant words, not a similar topic), it is held as **Possible duplicate** before any model run. A person answers on the ticket page: close it as a duplicate, or handle it as a new ticket (it is then never proposed again). The model never decides this.

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
| `TASKLANE_MAX_DEV` | no | Development runs at once, default 1. Raise it only after you measure your CI capacity. |
| `TASKLANE_USER_DAILY_LIMIT` | no | Reports per reporter per day, default 10. |
| `TASKLANE_SYNC_SECS` | no | Polling interval for the sync backstop, default 60, minimum 5. |
| `TASKLANE_SOURCE_NAME` | no | Registry name of the source, default `github`. |
| `TASKLANE_RETENTION_DAYS` | no | Audit rows older than this are deleted (default 365, minimum 7). Finished jobs are deleted after 30 days. |
| `TASKLANE_ALLOW_NO_CHECKS` | no | `1` treats a pull request with no CI checks as green. Default off. |

The agent-engine plug-in has its own switch, `TASKLANE_ALLOW_UNSANDBOXED=1`, explained in section 7. Plug-in specific variables are documented in `plugins/source-github/README.md` and `plugins/engine-claude/README.md`.

## 3. How a repository becomes active

1. The sync job lists every repository the App can see. New ones start in **triage** mode (archived ones in **watch**).
2. New issues in a **triage** repository are read and planned by the agent. Nothing is changed until a person approves a ticket; the **first approval turns the repository to full mode** automatically.
3. In **full** mode, a ticket that the rules and the agent both rate level 1 runs through to a merge without a person, **if its author is a collaborator** (owner, member or collaborator of the repository, or tasklane itself acting for a reporter). An issue from anyone else always waits for approval. Levels 2 and 3 wait for approval first.
4. Change a repository's mode at any time in the console, under "Repositories". **watch** stops all activity for it.

Issues that existed before you installed the App are not imported (the first poll only sets a baseline). Create a new issue, or use the report button.

## 4. Approving, pausing, failing

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
   one supervised dry run against live GitHub and live Claude on a throwaway private repository: unattended issue-to-merge twice, a protected-path change that stopped for approval of its exact diff, the pause switch, the question flow, and GitHub's refusal of a workflow push. That run found three bugs, all fixed. A second supervised run used the one-click App connection, a Claude subscription token and the Docker sandbox (on Docker's default `bridge` network) against a private sandbox repository: issue to merged pull request, an executable file included, with the reviewer catching a missing executable bit. It found four more bugs, all fixed (the sandbox user could not write the checkout, Retry could merge a pull request the reviewer had not approved, one blocked repository stopped the whole sync, the developer could not set the executable bit). Two installs on one repository were run side by side against real GitHub: the second skipped the issue and the first handled it. GitHub webhooks were also run for real, through a temporary tunnel that forwarded only the webhook path: a new issue and a new comment each reached tasklane as a signed delivery from GitHub and the ticket appeared at once. (The webhook was set on the repository, because the App created by the one-click flow has no webhook or events when tasklane has no public address; start tasklane with `TASKLANE_PUBLIC_URL` set to an `https://` address before pressing Connect GitHub and the App is created with the webhook and its events.) **What has not run for real:** Telegram (tested against a fake API only), and anything with more than one approver. Do your own supervised dry run on a throwaway repository first.
- The console signs in with one admin token and uses server-side sessions (8 hours, random id, per-session form token, sign-out, lockout after five wrong tokens). It has no user accounts: everyone who has the token is the owner.
- Logs go to stderr and contain job and ticket ids, not secrets. There are no metrics endpoints yet. A plug-in that crashes is started again on the next call; `/readyz` checks the database and that both plug-ins answer.

## 8. Backup, upgrade, troubleshooting

- **Backup:** `pg_dump` the database. The `/data` volume only holds re-creatable git mirrors.
- **Upgrade:** pull, rebuild, restart. Migrations are applied on start and never destructive.
- **Stuck job?** The reaper returns jobs whose worker died; a ticket that waits too long shows in the console. A paused system shows "paused" on the home page.
- **`readyz` fails:** the database is unreachable.
- **Nothing happens after installing the App:** check the repository's mode, that the issue is new (not older than the install), and the daemon log for `sync:` lines.
