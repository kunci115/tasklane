# source-github

Ticket-source plug-in for GitHub, authenticated as a GitHub App. Single file, Python 3.10+, standard library only (RS256 signing calls `openssl`, which must be on `PATH`). Speaks the JSON-lines protocol in `docs/design/05-plugin-protocol.md`; operations are in `06-plugin-operations.md`.

## Environment

| Variable | Meaning |
|---|---|
| `GITHUB_API_URL` | API base, default `https://api.github.com` |
| `GITHUB_APP_ID` | App id (JWT issuer) |
| `GITHUB_APP_PRIVATE_KEY_FILE` | path to the App's PEM private key |
| `GITHUB_INSTALLATION_ID` | optional; found via `/app/installations` if absent (matched to `GITHUB_ORG` when set) |
| `GITHUB_WEBHOOK_SECRET` | secret for `X-Hub-Signature-256` |
| `GITHUB_ORG` | optional; ignore repos and services of other owners |

## Credentials model

- Reading and writing issues and comments, `get_checks`, `detect_facts` and `list_*` use the installation token with the App's default permissions. It is cached until 60 s before expiry.
- `mint_run_token` creates a separate installation token limited to one repository and the requested permissions (for example `contents:write`, `pull_requests:write`). GitHub fixes its lifetime at about one hour; `ttl_secs` is accepted but cannot change that. `expires_at` is returned.
- `open_pr` and `merge_pr` use only the `token` passed in params, and fail if it is missing.
- Tokens, key contents and the webhook secret are never logged or put in errors.

## GitHub App setup

Repository permissions:

- Issues: read and write
- Pull requests: read and write
- Contents: read and write. Grant it on the App, but the plug-in only uses it through minted run tokens.
- Metadata: read
- Checks: read
- Commit statuses: read (for `get_checks`)
- Environments / Administration: read, optional (for `detect_facts` environments; absent means an empty list)

Subscribe to webhook events: Issues, Issue comment, Pull request, Check suite. Set the webhook secret to `GITHUB_WEBHOOK_SECRET`.

## Behaviour notes

- `list_changes`: the first call (no cursor) only sets a baseline of "now" and returns no items. Later calls use `since`, `If-None-Match` per repo and return the same cursor with empty items on 304. `since` is inclusive, so the newest item can repeat once; consumers must be idempotent. Archived repos are skipped.
- `merge_pr` re-reads the PR and returns `head_moved` without merging if `head.sha` differs. HTTP 409 maps to `head_moved`; 405 maps to `conflict`, `checks_failing` or `not_mergeable` by message.
- `get_checks` combines check runs and commit statuses; no checks at all counts as `success`.
- `verify_webhook` maps `issues`, `issue_comment`, `pull_request` and `check_suite` (labeled/unlabeled become kind `label`); other events are a `permanent` error.
- `repo_url` always uses `github.com`.
- HTTP errors: 401 and plain 403 are `unauthorized`; 429 or 403 with rate-limit headers is `rate_limited` (message gives the wait); 5xx and network errors are `transient`; other 4xx are `permanent`. Every request has a 30 s timeout.

## Tests

    python3 -m unittest discover -s plugins/source-github -v

They run a fake GitHub API on localhost and need `openssl` to make a throwaway key.
