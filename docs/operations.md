# Running tasklane

tasklane is a single daemon plus Postgres. It reads tickets from a ticket source (GitHub in stage 1), has an AI agent triage and fix them, opens pull requests, and merges to a development branch after CI and review. **People approve anything risky; the system never touches production.**

## What you need

- Docker with Compose (or a Linux host with Postgres 14+, git, python3, openssl, node and the Claude Code CLI).
- A GitHub App installed on your organization or account.
- An Anthropic API key (or a Claude Code OAuth token) for the agent.
- A TLS reverse proxy in front of port 8080 if the console or the webhook is reachable from outside.

## Quick start (Docker)

```bash
git clone https://github.com/kunci115/tasklane && cd tasklane
cp .env.example .env            # fill in the values (see below)
cp /path/to/your-app.private-key.pem github-app-key.pem
chmod 644 github-app-key.pem   # the container user (uid 10001) must be able to read it
docker compose -f compose.prod.yaml up -d --build
curl -fsS http://127.0.0.1:8080/readyz     # prints "ready"
```

Open the console at `http://127.0.0.1:8080/` (through your proxy in production) and sign in with `TASKLANE_ADMIN_TOKEN`.

## 1. Create the GitHub App

GitHub: Settings, Developer settings, GitHub Apps, New. Then:

- Webhook URL: `https://<your host>/api/v1/webhook/source`, with a random webhook secret (`GITHUB_WEBHOOK_SECRET`).
- Repository permissions: Issues (read and write), Pull requests (read and write), Contents (read and write), Metadata (read), Checks (read), Commit statuses (read). Environments/Administration (read) is optional.
- Subscribe to: Issues, Issue comment, Pull request, Check suite.
- Generate a private key (PEM) and note the App id; install the App on your organization or account and note the installation id (the number in the installation URL).

The App never holds write access in the agent's hands: the daemon asks GitHub for a separate token limited to one repository and valid for about an hour each time it pushes or merges.

## 2. Configuration

Everything is an environment variable. The daemon refuses to start with a clear message when a required value is missing or weak.

| Variable | Required | Meaning |
|---|---|---|
| `TASKLANE_DATABASE_URL` | yes | Postgres URL. Migrations run on start. |
| `TASKLANE_ADMIN_TOKEN` | yes | At least 24 characters. Signs in to the console. |
| `TASKLANE_SOURCE_CMD` | yes | Command that starts the ticket-source plug-in. The image sets the GitHub one. |
| `TASKLANE_ENGINE_CMD` | yes | Command that starts the agent-engine plug-in. The image sets the Claude one. |
| `TASKLANE_SOURCE_ENV` / `TASKLANE_ENGINE_ENV` | no | Comma-separated names of variables to pass to that plug-in. **Nothing else is inherited**, apart from `PATH`. |
| `TASKLANE_CHAT_CMD`, `TASKLANE_CHAT_ENV` | no | Chat plug-in (see "Chat"). |
| `TASKLANE_BIND` | no | Listen address, default `127.0.0.1:8080` (`0.0.0.0:8080` in the image). |
| `TASKLANE_PUBLIC_URL` | no | If it starts with `https://`, the session cookie is marked `Secure`. |
| `TASKLANE_DATA_DIR` | no | Git mirrors and worktrees, default `./data` (`/data` in the image). Keep it on a volume. |
| `TASKLANE_JWT_SECRETS` | no | `name:secret,name:secret` (secrets of 16+ characters), one per app that embeds the report button. Without it the report endpoint refuses everyone. |
| `TASKLANE_WORKERS` | no | Parallel workers, default 2. |
| `TASKLANE_MAX_DEV` | no | Development runs at once, default 1. Raise it only after you measure your CI capacity. |
| `TASKLANE_USER_DAILY_LIMIT` | no | Reports per reporter per day, default 10. |
| `TASKLANE_SYNC_SECS` | no | Polling interval for the sync backstop, default 60, minimum 5. |
| `TASKLANE_SOURCE_NAME` | no | Registry name of the source, default `github`. |
| `TASKLANE_ALLOW_NO_CHECKS` | no | `1` treats a pull request with no CI checks as green. Default off. |

Plug-in specific variables are documented in `plugins/source-github/README.md` and `plugins/engine-claude/README.md`.

## 3. How a repository becomes active

1. The sync job lists every repository the App can see. New ones start in **triage** mode (archived ones in **watch**).
2. New issues in a **triage** repository are read and planned by the agent. Nothing is changed until a person approves a ticket; the **first approval turns the repository to full mode** automatically.
3. In **full** mode, a ticket that the rules and the agent both rate level 1 runs through to a merge without a person. Levels 2 and 3 wait for approval first.
4. Change a repository's mode at any time in the console, under "Repositories". **watch** stops all activity for it.

Issues that existed before you installed the App are not imported (the first poll only sets a baseline). Create a new issue, or use the report button.

## 4. Approving, pausing, failing

- A ticket that needs a person shows as **waiting for approval** in the console and carries a comment on the issue. The approval is bound to a hash of the exact plan or diff shown; if the plan changes, the old approval is refused.
- Levels: 1 runs by itself, 2 needs a `team_approver` or `owner`, 3 needs an `owner`. The console admin is an owner. Add people in the `principals` table (`channel`, `ref`, `role`).
- **Pause everything** in the console stops all job pickup instantly; running jobs finish their current step. Resume restarts.
- A ticket that cannot continue (CI red twice, the agent changed nothing three times, a branch that moved under review) goes to **failed**, with the reason on the ticket. Nothing retries forever.

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

Anyone in a registered group can read what `/ask` can read from that repository. Keep groups private and watch who joins.

## 7. Security model and honest limits

- Agents hold no tokens, never push, never merge. The daemon pushes and merges with per-run tokens.
- Risk levels come from fixed rules on the ticket text and the real diff (protected paths, size); the model can raise a level, never lower it. The rules are starting defaults in `crates/tasklane-core/src/floor.rs` and are deliberately broad.
- Ticket text, comments, diffs and chat messages are treated as untrusted input.
- **The development agent runs code-editing tools on your server. By default (`TASKLANE_SANDBOX` unset) it runs as the same user and in the same container as the daemon, which holds the database URL and tokens in its environment and files. Do not run untrusted repositories or untrusted reporters that way.** Use the Docker sandbox mode of the engine plug-in (`plugins/engine-claude/README.md`) with an egress-limited network, or run tasklane on a host that holds nothing else.
- The shipped plug-ins are tested against fakes (a fake GitHub API, a fake Claude CLI, a fake Telegram API). Nothing in this repository has been verified against the live services; do a supervised dry run on a throwaway repository first.
- Logs go to stderr and contain job and ticket ids, not secrets. There are no metrics endpoints yet.

## 8. Backup, upgrade, troubleshooting

- **Backup:** `pg_dump` the database. The `/data` volume only holds re-creatable git mirrors.
- **Upgrade:** pull, rebuild, restart. Migrations are applied on start and never destructive.
- **Stuck job?** The reaper returns jobs whose worker died; a ticket that waits too long shows in the console. A paused system shows "paused" on the home page.
- **`readyz` fails:** the database is unreachable.
- **Nothing happens after installing the App:** check the repository's mode, that the issue is new (not older than the install), and the daemon log for `sync:` lines.
