# Changelog

## 0.0.1 (unreleased)

First pre-release. Triage, development, pull request, CI wait, review and merge run end to end in tests against a real Postgres and git, with fake GitHub, Claude and Telegram services. Not yet run against the live services.

Hardening from an independent review before the first tag: protected-path floor that cannot be sidestepped (non-ASCII names, renames, nested paths, empty lists), approvals bound to the exact diff and shown to the approver, text re-scored before development, git never runs agent-controlled config, tokens passed by environment, collaborator-only unattended work, console sessions on the server with sign-out and lockout, sandbox gate for the engine, failed tickets recoverable from the console, dead plug-ins restarted, stuck tickets failed by the reaper, retention.
