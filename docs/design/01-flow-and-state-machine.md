# 01. Ticket flow and state machine

Status: draft 1. Stage 1 = GitHub as ticket source, Claude as agent engine. Words like "ticket source" and "agent engine" are plug-in roles (see `03-plugin-interfaces.md`).

## 1. Principles

1. **The ticket source is the source of truth for tickets.** The database holds only what the source cannot: queue, leases, approvals, audit, registry.
2. **Agents think, runners act.** Agents produce text and diffs inside a sandbox. Only runners push, open PRs and merge, using short-lived per-run tokens.
3. **Humans approve, never time.** No state advances because a timer ran out. A timer can only remind, escalate to a human, or close a ticket as *not done*.
4. **Every step is resumable.** State is written before work starts; a crashed worker's job is picked up again from its last checkpoint.
5. **Level is computed, not asked.** Risk level comes from deterministic rules on paths, diff and action tables; an LLM may raise it, never lower it.

## 2. Ticket states

| State | Meaning | Entered by | Left by |
|---|---|---|---|
| `new` | Ticket seen, not yet looked at | sync / gateway / chat bridge | orchestrator claims it |
| `needs_info` | Triage asked the reporter a question | triage | reporter replies, or 14 days with no reply, then `closed_stale` |
| `duplicate_proposed` | Triage proposes "same as X" | triage | a human confirms (`closed_duplicate`) or rejects (`triaged`) |
| `triaged` | Plan + level computed | triage + escalation | level L1 goes to `queued`; L2/L3 goes to `awaiting_approval` |
| `awaiting_approval` | Waiting for a human with the right role | escalation | approval button, or reject, or 30 days then `closed_stale` (never auto-approved) |
| `queued` | Approved, waiting for a free slot | approval or L1 | admission control admits it |
| `developing` | Agent working in a sandbox | admission | diff produced, or agent failed |
| `rescoring` | Runner scores the real diff | diff produced | level unchanged goes on; level raised goes back to `awaiting_approval` |
| `pr_open` | Branch pushed, PR open | PR runner | CI starts |
| `ci_running` | CI lane running | PR runner | green, or red |
| `reviewing` | Reviewer agent reads the diff | CI green | verdict |
| `merge_ready` | Reviewed head SHA recorded | reviewer approve | merge runner |
| `merged` | Merged to the dev/staging branch | merge runner | a human promotes (outside the system) |
| `failed` | Retries used up | any worker state | human decides: retry, edit, close |
| `stopped` | Console stop switch | console | console resume |
| `closed_*` | `closed_stale`, `closed_duplicate`, `closed_rejected`, `closed_done` | various | terminal |

Promotion to production is **not a state**. The system never touches production.

## 3. Happy path

```
new -> triaged -> (awaiting_approval ->) queued -> developing -> rescoring
    -> pr_open -> ci_running -> reviewing -> merge_ready -> merged
```

Step by step:

1. **Intake.** A report arrives through the gateway (signed login token), a chat message, or a ticket typed on the source. The gateway checks the signature, applies per-user limits, looks for duplicates, and creates a ticket on the source. Attachments go to private storage behind login.
2. **Sync.** The sync job watches the source (webhook first, conditional polling as a backstop), discovers repos, fills the service registry, and enqueues a job for every new or changed ticket.
3. **Triage** (read-only agent). Reads the ticket and the repo. Output: plan, estimated size, questions for the reporter, optional duplicate proposal. It runs in a sandbox with no secrets and no write access.
4. **Escalation.** The level is computed: fixed path/pattern rules are the floor (database migrations, authentication and roles, secrets, deploy and infrastructure, payments, message templates, deleting or altering data, personal data). The LLM can only raise it. L1 = automatic, L2 = team approver, L3 = owner. Output: level + reasons, written to the ticket.
5. **Approval gate.** For L2/L3, an approval request is posted on the ticket and to the chat channel with buttons. Only mapped users with the right role count. The approval is stored with who, what level, which plan hash, and when.
6. **Admission.** The dev step starts only if the CI lane queue is shallow and fewer than K dev runs are active (K = 1 at start). Otherwise the ticket stays `queued`.
7. **Develop** (dev agent). Container with a git worktree from a bare mirror; no secrets; egress allowlist (agent engine endpoint and the mirror only). Output: a diff and a summary. Nothing is pushed by the agent.
8. **Re-score.** The PR runner scores the real diff with the same deterministic rules. If a protected path is touched or the diff is much larger than the plan, the level goes up and the ticket returns to `awaiting_approval`.
9. **PR.** The PR runner mints a token scoped to one repo for this run, pushes the branch, and opens the PR.
10. **CI** runs in an isolated lane (containers, no secrets). Red twice = `failed` (a human takes over). Green goes on.
11. **Review** (read-only agent). Reads the diff and CI result. Verdict: approve, request changes, or escalate. Changes requested send the ticket back to `developing` (counts against the retry budget).
12. **Merge.** The merge runner merges only the reviewed head SHA, and only if branch rules (required checks) pass server-side. If the head moved since the review, it refuses.
13. **Status.** Every transition posts a short status on the ticket and to the reporter's channel.

## 4. Retry and budget rules

| Budget | Default | On exhaustion |
|---|---|---|
| CI runs per ticket | 2 | `failed`, human takes over |
| Developing attempts per ticket | 3 | `failed` |
| Model tokens per ticket | configurable cap | `failed` with a clear reason |
| Wall time per state | configurable | reaper frees the lease; counts as an attempt |

## 5. Claim table and leases

Jobs live in Postgres. Workers claim with `FOR UPDATE SKIP LOCKED` so two workers never take the same job, and without a coordinator.

```sql
-- claim one job
UPDATE jobs
SET lease_owner = $1,
    lease_until = now() + interval '120 seconds',
    fence       = fence + 1,
    attempts    = attempts + 1,
    status      = 'running'
WHERE id = (
  SELECT id FROM jobs
  WHERE status = 'queued' AND run_after <= now()
  ORDER BY priority DESC, run_after
  FOR UPDATE SKIP LOCKED
  LIMIT 1
)
RETURNING *;
```

- **Heartbeat** every 30 s: `UPDATE jobs SET lease_until = ... WHERE id = $1 AND fence = $2 AND lease_owner = $3`. Zero rows updated = the lease was lost; the worker stops.
- **Fencing token.** Every write a worker makes (state change, checkpoint, audit row) includes `fence`. A stale worker whose lease expired cannot overwrite the new owner's work.
- **Reaper** (runs every minute): jobs with `status = 'running' AND lease_until < now()` go back to `queued` with exponential `run_after`, or to `failed` if attempts are used up. It also removes orphaned worktrees and containers.
- **Checkpoints.** After each state the orchestrator stores a small JSON checkpoint, so a resumed job continues from there.
- **Kill switch.** The console sets `status = 'stopped'` on a lane or a ticket; workers check on each heartbeat.

## 6. Admission control

Before `developing` starts: `active_dev_runs < K` and `ci_queue_depth < T`. `K = 1` and `T` = number of CI runners at start. These are configuration; measure before raising them (see the measurement plan in `README.md`).

## 7. Chat flow (Ask and Task)

- A chat group is registered to one service. The bot ignores and leaves unregistered groups.
- **Ask:** any member can question the service's agent. It runs read-only, answers from the repo, tickets and memory, then ends. Secrets are stripped from answers. Each question is logged and counted against a per-group daily cap.
- **Task:** any member can send `/task ...`; it creates a normal ticket and follows the flow above.
- **Approvals** in chat are buttons pressed by mapped users with a role. Typed text such as "ok" never approves.

## 8. Device actions

Device actions are not tickets' code changes. They follow `04-threat-model.md` and `03-plugin-interfaces.md` (device adapter): the broker assigns the level from a fixed action table, L2/L3 need an approval token signed by the gate and bound to the exact device and parameters with an expiry, and a kill switch disables all device actions.
