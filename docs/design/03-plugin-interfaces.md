# 03. Plug-in interfaces

Status: draft 1. These are contracts, not code: operations, inputs, outputs and rules. The implementation language is still open (see `README.md`).

General rules for every plug-in:

- A plug-in is stateless between calls. Anything it must remember is stored by the core.
- Every call has a timeout and returns a typed error (`transient`, `permanent`, `unauthorized`, `rate_limited`). The core decides about retries.
- A plug-in never receives secrets it does not need, and never logs them.
- Stage 1 ships exactly one implementation of the first two: `github` and `claude`.

## 1. TicketSource

Reads and writes tickets, repos and pull requests on a hosting service.

| Operation | Input | Output |
|---|---|---|
| `list_services()` | none | services visible to the install (repos) with default branch and archived flag |
| `list_changes(since)` | cursor | changed items since the cursor, plus the next cursor (uses conditional requests where the source allows) |
| `get_item(ref)` | ticket ref | title, body, labels, author, comments, attachments (as links) |
| `create_item(service, title, body, labels)` | | new ref |
| `comment(ref, text)` | | comment id |
| `set_labels(ref, add, remove)` | | none |
| `detect_facts(service)` | | language, test command guess, environments, protected paths |
| `open_pr(service, branch, title, body)` | token scoped to one service | PR ref |
| `get_checks(pr)` | | list of check results and the head SHA |
| `merge_pr(pr, expected_head_sha)` | token scoped to one service | merged, or `conflict` / `head_moved` / `checks_failing` |
| `verify_webhook(headers, body)` | | event or reject |
| `mint_run_token(service, permissions, ttl)` | | short-lived token limited to one service |

Rules: `merge_pr` must refuse if the head SHA differs from `expected_head_sha`. Intake and runner credentials are separate: reading and creating issues must not need write access to code.

## 2. AgentEngine

Runs one agent task and returns text. It does not touch git, tickets or devices itself.

| Operation | Input | Output |
|---|---|---|
| `run(task)` | profile (system prompt, allowed tools), prompt, workspace path, `tools_allowed`, budget (tokens, wall time), network allowlist | final text, structured result if requested, usage (tokens, time), exit reason |
| `cancel(run_id)` | | none |
| `capabilities()` | | supports tool use, structured output, read-only mode, max context |

Rules: read-only roles (triage, reviewer, Ask) run with no write tools and no shell. The dev role runs in a container the core created; the engine is told the workspace and nothing else. The engine receives no secrets. Budget exhaustion is a normal result (`exit_reason = budget`), not a crash.

## 3. Notifier

One-way status messages to people.

| Operation | Input | Output |
|---|---|---|
| `send(recipient_ref, text, link?)` | opaque recipient from the gateway token or chat | delivered / failed |

## 4. ChatChannel

Two-way chat (Telegram first).

| Operation | Input | Output |
|---|---|---|
| `receive()` | webhook or poll | normalized events: message, command (`/ask`, `/task`), button press, member joined/left |
| `send(group_or_user, text, buttons?)` | | message id |
| `leave(group)` | | none |

Rules: unregistered groups are ignored and left. Button presses carry the pressing user's id; the core maps it to a role. Free-text replies never count as approvals. A "member joined" event in a registered group raises an alert.

## 5. DeviceAdapter

Talks to one kind of device over the network. Runs inside the broker's isolated network namespace, never inside an agent container.

| Operation | Input | Output |
|---|---|---|
| `describe()` | none | the list of actions it supports, each with name, params schema, reversibility and suggested level |
| `health(device)` | device record | status, last seen, metadata only |
| `execute(device, action, params, credentials)` | validated params | result with raw output redacted per adapter rules |

Rules:

- The **core table**, not the adapter, decides each action's level. `describe()` only suggests it.
- Adapters receive an address from the device record, never from a caller. No URLs or hosts in params.
- Every L2 action names its inverse. Every action respects per-device rate limits and time windows from the table.
- Raw camera video or personal data is never returned to agents or stored in memory. Metadata only until the owner decides otherwise.

## 6. Memory (optional, shared)

| Operation | Input | Output |
|---|---|---|
| `recall(scope, query)` | scope = one service | matching notes |
| `remember(scope, fact, provenance)` | | id |

Rules: scope is per service. A chat group or agent can read only its own service's scope. Stage 1 uses a built-in Postgres table; an external memory service can replace it.

## 7. Versioning

Each interface has a version number. The core refuses to load a plug-in that declares an unknown major version. Adding an operation is a minor version; changing or removing one is a major version.
