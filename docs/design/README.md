# Design documents

Status: draft 1. Stage 1 = GitHub as ticket source, Claude as agent engine, one organization per install.

| Document | Content |
|---|---|
| [01-flow-and-state-machine.md](01-flow-and-state-machine.md) | Ticket flow, every state, retries and budgets, claim table and leases, admission control, chat and device flows |
| [02-schema.md](02-schema.md) | Postgres tables |
| [03-plugin-interfaces.md](03-plugin-interfaces.md) | Contracts for ticket source, agent engine, notifier, chat channel, device adapter, memory |
| [04-threat-model.md](04-threat-model.md) | Assets, trust boundaries, 12 threats with controls and residual risk, tests owed |

Diagrams: [`../architecture`](../architecture).

## Measurement plan (before we promise any numbers)

1. Run 1, 2 and 4 CI runs at once and read the real wall time.
2. Time worktree creation plus dependency setup for a typical repo.
3. Count source API calls for one discovery sweep over N repos.
4. Run triage, dev and review on 5 old issues and count tokens.
5. Test duplicate detection on about 50 old issues: keywords only, trigram, and model compare.
6. Measure how often CI is red on a clean main branch (flake rate).

## Open questions

1. Implementation language and queue/workflow library (a custom Postgres queue is assumed in these documents).
2. Whether the state machine uses a workflow library or plain code with the checkpoint table.
3. Exact set of protected paths shipped as defaults.
4. How the setup wizard stores the ticket source and engine credentials.
5. Whether video is ever in scope for device adapters.
