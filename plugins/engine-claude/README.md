# engine-claude

Agent engine plug-in for tasklane. It speaks the JSON-lines plug-in protocol
(`docs/design/05-plugin-protocol.md`, `06-plugin-operations.md`) and runs the Claude Code CLI
headless. Python 3.10+, standard library only. Operations: `describe`, `capabilities`, `run`.

## Roles

| Role | Tools | Result |
|---|---|---|
| triage | `Read, Grep, Glob` only (`--tools`), Bash/Edit/Write/Web denied | `level` 1..3 (1 trivial/safe, 2 team approval, 3 owner), `plan`, `questions` |
| dev | Read, Edit, Write, Grep, Glob, plus Bash limited to `git status/diff/log` and `profile.test_command` (exact match); WebFetch/WebSearch/curl/wget/git push/git commit denied; cwd = `params.workspace` | `summary` |
| reviewer | read-only, gets `params.diff` | `verdict` approve/changes/escalate, `notes` |
| ask | read-only | `answer` |

All results carry `usage {tokens_in, tokens_out, secs}` and `exit_reason`. `read_only: true` on any
role forces the read-only tool set. Permission mode is always `dontAsk` (anything not allowed is
refused, nothing prompts). Structured roles also pass `--json-schema`, and the output is validated
again here: bad or missing fields give a `permanent` error "unusable model output".

Prompt design: ticket text, comments, diff and question are untrusted. They go in the user prompt
(stdin) inside `<ticket>`, `<comment>`, `<diff>`, `<question>` blocks; those tag names are escaped
inside the text so it cannot close a block. Fixed rules (data not instructions, no secrets, no
network, no commit/push) are in `--append-system-prompt`, followed by `profile.system`.

## Environment variables

| Variable | Meaning |
|---|---|
| `TASKLANE_CLAUDE_BIN` | CLI to run, default `claude` (tests point it at a fake) |
| `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` | model credentials, forwarded if present, never printed |
| `TASKLANE_SANDBOX` | unset/`none` (direct) or `docker` |
| `TASKLANE_SANDBOX_NETWORK` | docker network name; required in docker mode |
| `TASKLANE_SANDBOX_IMAGE` | image containing the `claude` CLI; required in docker mode |
| `TASKLANE_SANDBOX_CLAUDE_BIN` | CLI path inside the image, default `claude` |
| `TASKLANE_SANDBOX_MEMORY` / `_PIDS` | container limits, default `2g` / `256` |
| `TASKLANE_DOCKER_BIN` | docker client, default `docker` |

The CLI child gets only `PATH`, `HOME` (a fresh temp dir removed after each run) and the credential
variables above. Nothing else is inherited. In docker mode the docker client also gets `DOCKER_HOST`
if set.

## Sandbox modes

- **none (default)**: the CLI runs directly. On start the plug-in prints
  `engine-claude: dev role is NOT sandboxed` on stderr. The dev role then relies only on the CLI's
  own permission rules. Use docker mode for anything real.
- **docker**: `docker run --rm -i --name tasklane-<id> --network $TASKLANE_SANDBOX_NETWORK
  --cap-drop ALL --security-opt no-new-privileges --read-only --tmpfs /tmp --tmpfs /home/agent
  --pids-limit --memory --user 10001:10001 -e HOME=/home/agent -e ANTHROPIC_API_KEY ...`.
  Only the workspace is mounted, at `/work` (read-write for dev, read-only for reviewer/ask; none for
  triage). Credentials are passed by name so the value is not in argv. The **operator** must create
  the docker network with an egress allowlist that reaches only the model API; this plug-in cannot
  enforce that. Docker mode refuses every run (not only dev) if the network or image is unset, since
  every role needs to reach the model. On timeout the container is also `docker kill`ed.

## Budget and errors

`budget.max_secs` is a hard wall-clock timeout (default 900); the whole process group is killed. If
partial output is already usable it is returned with `exit_reason: "budget"`, otherwise a
`transient` error with `exit_reason: "budget"`. `budget.max_tokens` is NOT enforced: the CLI has no
token cap flag. CLI failures map to `unauthorized`, `rate_limited` or `transient` by message
heuristics; a missing binary or bad params are `permanent`. Requests are handled one at a time.

## CLI flags used

`-p`, `--output-format json`, `--permission-mode dontAsk`, `--tools`, `--allowedTools`,
`--disallowedTools`, `--append-system-prompt`, `--json-schema`, `--model`, `--strict-mcp-config`,
`--disable-slash-commands`, `--no-session-persistence`. The installed CLI has no `--max-turns`
flag, so it is not used.

## Honest limits

- Everything was tested against a FAKE `claude` script (`python3 -m unittest discover -s .`). No
  real model call or real docker run was made. The real CLI's JSON output shape (`result`, `usage`,
  `duration_ms`, `is_error`, `structured_output`) is parsed defensively but is unverified end to end.
- Docker argv is asserted, not executed. Image contents and the egress-allowlist network are the
  operator's job.
- Prompt injection is mitigated (delimiters, rules, read-only tools, no network in the sandbox) but
  not solved: a dev run can still write a bad diff. Review and CI remain the defence.
- Direct mode does not stop the dev role reaching the network through allowed tools beyond what
  the CLI permission rules block.
- The model provider sees the prompts (threat model T6).

## Sandbox gate

Every role refuses to run unless `TASKLANE_SANDBOX=docker` or `TASKLANE_ALLOW_UNSANDBOXED=1` (exactly `1`). Without the sandbox the CLI runs as the same user as its parent and can read the parent's environment and files. A reviewer asked to judge a diff longer than 200,000 characters answers `escalate` instead of approving something it did not fully read. Results have known token shapes (GitHub, Anthropic, AWS, private keys, URLs with credentials) and configured credentials removed.
