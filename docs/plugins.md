# Writing a plug-in

tasklane talks to everything outside itself through plug-ins: a **ticket source** (GitHub today), an **agent engine** (Claude today), and a **chat channel** (Telegram today). A plug-in is a separate program in any language. The core starts it, sends it one JSON request per line on stdin, and reads one JSON response per line on stdout. Logs go to stderr and are never parsed.

```
-> {"id": 1, "op": "describe"}
<- {"id": 1, "ok": true, "result": {"name": "file", "role": "ticket_source", "protocol_major": 1, "version": "0.1.0"}}
-> {"id": 2, "op": "get_item", "params": {"service": "acme/shop", "ref": "1"}}
<- {"id": 2, "ok": false, "error": {"kind": "permanent", "message": "unknown ticket"}}
```

## The rules every plug-in follows

1. **`describe`** always works and returns `name`, `role`, `protocol_major` and `version`. The core refuses a plug-in whose `protocol_major` it does not know (it is `1`).
2. Every response echoes the request's `id`. Responses may come in any order.
3. Errors have a `kind`: `transient` (try again later), `permanent`, `unauthorized`, or `rate_limited`. The core decides about retries, not the plug-in. A `rate_limited` error may carry `retry_after_secs`: the core then waits that long (60 to 3600 s) and runs the job again without counting an attempt.
4. **Expected business outcomes are results, not errors.** A merge refused because the head moved is `{"merged": false, "reason": "head_moved"}`.
5. **Tokens and keys passed in `params` are secrets.** Never log them and never put them in error messages. Tests should check for that (the GitHub plug-in's tests have a leak check to copy).
6. Never crash the read loop: catch every exception and answer with an error. A line that is not JSON is ignored.
7. The core gives a plug-in **only the environment variables listed for it** (`TASKLANE_SOURCE_ENV`, `TASKLANE_ENGINE_ENV`, `TASKLANE_CHAT_ENV`), nothing else.
8. A plug-in that stops answering is killed after the call's timeout and started again on the next call.

The exact operations and JSON shapes for every role are in [docs/design/06-plugin-operations.md](design/06-plugin-operations.md); the contracts and their rules are in [docs/design/03-plugin-interfaces.md](design/03-plugin-interfaces.md).

## Start from the template

[`plugins/_template`](../plugins/_template) is a complete, tested ticket source whose tickets live in one JSON file. It implements the operations that let tasklane read tickets and comment (`describe`, `list_services`, `list_changes`, `get_item`, `comment`), which is enough to run a repository in `triage` mode.

1. Copy the directory to `plugins/source-yours/` and rename the files.
2. Replace the file access with calls to your tracker's API. Keep the structure: a table of operations, one function each, a `handle` that turns every failure into an error response.
3. Add the rest of the ticket-source operations when you want tasklane to write code in that tracker: `repo_url`, `mint_run_token`, `open_pr`, `get_checks`, `merge_pr`, and `verify_webhook` for webhooks. Optional: `get_pull` (`{service, pr}` gives `{base, head_sha, state, draft, title, body}`; the review needs it for the target branch). Optional: `list_pulls` (`{service}` gives `{pulls: [{number, title, author, bot, draft, branch, base, url, updated_at, ours}], truncated}`, the open pull requests, for the Pull requests page). Optional: `get_user` (`{login}` gives `{id}` of a person's account) lets tasklane credit people in commits (GitHub only). `merge_pr` must refuse (`head_moved`) when the pull request's head is not the commit that was reviewed.
4. Write tests that talk to the program over stdin and stdout, with a fake of your tracker's API (see `plugins/source-github/test_source_github.py`). CI runs `python -m unittest discover` in every directory under `plugins/`.

## Register it

Start tasklane with your program as the ticket source:

```bash
export TASKLANE_SOURCE_CMD="python3 -I /opt/tasklane/plugins/source-yours/source_yours.py"
export TASKLANE_SOURCE_ENV=YOURS_TOKEN,YOURS_URL      # the only variables it will see
export TASKLANE_SOURCE_NAME=yours                     # how repositories are labelled in the database
```

Then run `tasklane doctor`. It starts the plug-in, calls `describe`, and tells you if it does not answer. An engine or a chat plug-in is registered the same way with `TASKLANE_ENGINE_*` and `TASKLANE_CHAT_*`.

## What a good plug-in does

- **Conditional requests** where the source allows them, so an unchanged source costs almost nothing (`list_changes` returns the same cursor and no items).
- **Short-lived, narrow credentials.** `mint_run_token` returns a token limited to one repository that expires in minutes.
- **Atomic writes** for anything it stores, so a crash never leaves a half-written file.
- **Stays within its limits:** size limits on text it accepts, timeouts on every call it makes, and no following of redirects to other hosts.

## Contributing one

A new ticket source (GitLab, Gitea, Jira) or agent engine is the most useful contribution. Open an issue first to say which one so work is not duplicated, keep the plug-in standard-library only if you can, and include tests and a short section in the operations guide for what to set and what has been verified against the real service. See [CONTRIBUTING.md](../CONTRIBUTING.md).
