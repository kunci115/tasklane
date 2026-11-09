# Changelog

## Unreleased (0.1.0 is planned, not tagged)

A pre-release for supervised use. It will be tagged when the owner says so.

New console: overview with live board, ticket page with stepper, timeline and colour diff, repositories with agent settings, people and roles, health and setup checklist, activity feed, light and dark, mobile layout, installable as an app.

Live dry run (a throwaway private repository, live GitHub App, live Claude, about 10 minutes of tokens): an issue went to a merged pull request unattended twice in about 70 seconds to 2 minutes; a request to change a workflow file stopped for approval of the plan and then of the exact diff, and GitHub itself refused the push without the Workflows permission, which tasklane reported clearly instead of retrying; the pause switch stopped all work; a vague issue produced one set of questions. The run found and fixed three bugs that fakes could not show: GitHub strips hidden markers from comments (own comments are now recognised by the bot's login), triage had no view of the repository (it now gets a read-only checkout and asks only when it cannot proceed), and tasklane's own plan comment tripped the risk rules.

Also in this release: a minimal sandbox image (`Dockerfile.sandbox`) and an entrypoint-independent sandbox command, a backlog cap for issues created directly on the source, and a readiness check that includes the plug-ins.

## 0.0.1

First pre-release. Triage, development, pull request, CI wait, review and merge run end to end in tests against a real Postgres and git, with fake GitHub, Claude and Telegram services. Not yet run against the live services.

Hardening from an independent review before the first tag: protected-path floor that cannot be sidestepped (non-ASCII names, renames, nested paths, empty lists), approvals bound to the exact diff and shown to the approver, text re-scored before development, git never runs agent-controlled config, tokens passed by environment, collaborator-only unattended work, console sessions on the server with sign-out and lockout, sandbox gate for the engine, failed tickets recoverable from the console, dead plug-ins restarted, stuck tickets failed by the reaper, retention.
