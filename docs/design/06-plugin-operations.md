# 06. Plug-in operations (wire shapes)

Status: v1. This is the exact JSON for every operation in `03-plugin-interfaces.md`, carried by the protocol in `05-plugin-protocol.md`. `params` and `result` are JSON objects. A `service` is the source's name for a repository, such as `owner/name`. A `ref` is the source's id for a ticket, as a string.

Errors use `kind`: `transient`, `permanent`, `unauthorized`, `rate_limited`. Expected business outcomes (a merge refused because the head moved) are **results**, not errors.

## Common

| Op | Params | Result |
|---|---|---|
| `describe` | none | `{name, role, protocol_major: 1, version}` |

## ticket_source

| Op | Params | Result |
|---|---|---|
| `list_services` | `{}` | `{services: [{external_id, default_branch, archived: bool, private: bool}]}` |
| `list_changes` | `{cursor?: string}` | `{items: [{service, ref, kind: "issue"\|"comment"\|"label"\|"pr", updated_at}], cursor: string}`. Uses conditional requests where the source allows; an unchanged source returns `items: []` and the same cursor. |
| `get_item` | `{service, ref}` | `{title, body, author, labels: [string], state: "open"\|"closed", comments: [{author, body, at}]}` |
| `create_item` | `{service, title, body, labels: [string]}` | `{ref}` |
| `comment` | `{service, ref, body}` | `{id}` |
| `set_labels` | `{service, ref, add: [string], remove: [string]}` | `{}` |
| `detect_facts` | `{service}` | `{language?: string, test_command?: string, environments: [string], protected_paths: [string]}` (best guesses; the registry can override) |
| `repo_url` | `{service, token?: string}` | `{url}`: a git URL; with a token it embeds the credential for one push or clone. Never logged. |
| `mint_run_token` | `{service, permissions: ["contents:write","pull_requests:write"], ttl_secs: int}` (also `contents:read` for fetching private repos; the form is `name:level`) | `{token, expires_at}`: limited to that one service, short-lived |
| `open_pr` | `{service, branch, base, title, body, token}` | `{pr}` (pull request id as string) |
| `get_user` | `{login}` | `{id, login}` (optional; a person's account only, never a bot or an organisation; used to credit people in the history) |
| `create_repo` | `{owner, name, description, private, template}` | `{external_id, html_url, default_branch, private, seeded, seed_error}` (optional; a new repository for the New project page, seeded with a README, a .gitignore and `.tasklane.yml`) |
| `get_pull` | `{service, pr}` | `{base, head_sha, state, draft, title, body}` (optional; the target branch is what a review compares against) |
| `in_branch` | `{service, sha, branch}` | `{in_branch}` (optional; is the commit part of the branch, for closing issues after promotion) |
| `close_item` | `{service, ref}` | `{closed}` (optional; closes the issue as completed) |
| `list_open_issues` | `{service}` | `{refs: [string], more}` (optional; open issues without pull requests, oldest first, for the console's "Import open issues") |
| `list_pulls` | `{service}` | `{pulls: [{number, title, author, bot, draft, branch, base, url, updated_at, ours}], truncated}` (optional; open pull requests, read only, for the console page) |
| `get_checks` | `{service, pr}` | `{head_sha, state: "pending"\|"success"\|"failure", checks: [{name, state}]}` |
| `merge_pr` | `{service, pr, expected_head_sha, token}` | `{merged: bool, sha?: string, reason?: "head_moved"\|"checks_failing"\|"conflict"\|"not_mergeable"}` |
| `verify_webhook` | `{headers: {name: value}, body_b64}` | `{event: {service, ref, kind}}` or a `permanent` error if the signature is wrong |

Rules: `merge_pr` must return `merged: false, reason: "head_moved"` when the PR head differs from `expected_head_sha`. Tokens passed in params are secrets: never log them or put them in error messages.

## agent_engine

| Op | Params | Result |
|---|---|---|
| `capabilities` | `{}` | `{tool_use: bool, structured_output: bool, read_only_mode: bool, max_context?: int}` |
| `run` | see below | see below |

`run` params:

```json
{
  "role": "triage | dev | reviewer | ask",
  "ticket": {"service": "owner/name", "ref": "12", "title": "...", "body": "...", "comments": []},
  "profile": {"system": "optional extra instructions for this service", "model": "optional"},
  "workspace": "/path/to/worktree (dev, reviewer, ask; absent for triage)",
  "diff": "unified diff text (reviewer only)",
  "question": "text (ask only)",
  "feedback": "text (dev only): why the previous attempt failed (CI output, reviewer notes)",
  "budget": {"max_secs": 900, "max_tokens": 200000},
  "read_only": true
}
```

`run` result, by role (all also carry `usage: {tokens_in, tokens_out, secs}` and `exit_reason: "ok" | "budget" | "error"`):

| Role | Result fields |
|---|---|
| `triage` | `level: 1..3`, `plan: string`, `questions: [string]` |
| `dev` | `summary: string` (the diff is read by the core from the workspace with `git`; the engine never pushes) |
| `reviewer` | `verdict: "approve"\|"changes"\|"escalate"`, `notes: string` |
| `ask` | `answer: string` |

The engine must run `triage`, `reviewer` and `ask` read-only (no write tools, no shell). `dev` may write only inside `workspace`. The engine receives no secrets except its own model credential, which it reads from its own environment.

## notifier

| Op | Params | Result |
|---|---|---|
| `send` | `{recipient_ref, text, link?}` | `{delivered: bool}` |

## chat_channel

| Op | Params | Result |
|---|---|---|
| `receive` | `{timeout_secs}` | `{events: [{kind: "message"\|"command"\|"button"\|"joined"\|"left", group, user, text?, command?, args?, data?}]}` (long poll; empty on timeout) |
| `send` | `{group, text, buttons?: [{label, data}]}` | `{message_id}` |
| `leave` | `{group}` | `{}` |

## Versioning

`protocol_major` is 1. New optional fields may be added in a minor version; the core ignores unknown fields and plug-ins ignore unknown params.
