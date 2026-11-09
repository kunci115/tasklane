# Autonomy: when the agents act alone and when they ask

tasklane can run fully on its own, always ask a person, or anything in between. You choose per repository, and a few rules decide per ticket. The model never decides how much freedom it gets: **fixed rules set the risk, and the model can only raise it.**

## The dial: three modes per repository

Set on the **Repositories** page, one repository at a time. New repositories start in `triage`.

| Mode | What tasklane does | When a person is needed |
|---|---|---|
| `watch` | Nothing. Issues are ignored. | Never (it is off). |
| `triage` | Reads the issue and the code and posts a plan, then **waits**. Once a person approves that plan, the agent writes the change and it goes on through CI, review and merge. An approval never changes the mode, so the next ticket asks again. | Always: every ticket waits for approval of its plan. |
| `full` | Plans, writes the change, opens the pull request, runs CI, has a second agent review, and **merges to your development branch by itself**. | Only when one of the rules below says so. |

## When a ticket runs on its own (the autonomous path)

In a `full` repository a ticket goes from issue to merge with nobody pressing anything only if **all** of these hold:

1. The issue's author is a collaborator, member or owner of the repository (or tasklane itself).
2. No risk rule matched the text, so the level is **L1**.
3. The change touches no protected path and is not larger than the limits (below).
4. Your CI is green on the exact commit, with no failed checks.
5. The reviewer agent approves that commit.
6. The repository's branch rules pass on the server.

Then tasklane merges **only the reviewed commit** into the development branch. Production is never touched; promoting it is yours.

## When a person is needed (escalation)

| Trigger | What happens | Who can approve |
|---|---|---|
| The ticket text mentions a database migration, login or permissions, deploy or infrastructure, message templates, or security or an outage | Level **L2** | team approver or owner |
| The text mentions secrets or credentials, payments or refunds, deleting or altering data, or personal data | Level **L3** | owner only |
| The model thinks the risk is higher than the rules | The level is raised to the model's | as above |
| The issue's author is a stranger (not a collaborator) | Always waits for a person, whatever the level | team approver or owner |
| The finished change touches a protected path (CI and deploy config, Dockerfiles and compose files, `migrations/`, `.env`, key files such as `.pem`, `CODEOWNERS`, and the paths you add) | Back to approval, **for that exact diff** | team approver or owner |
| The change is bigger than planned: more than 800 changed lines or 200 KB (lower with `max_diff_lines`) | Back to approval for that exact diff | team approver or owner |
| The reviewer agent answers "escalate" | Waits for a person | team approver or owner |
| The repository is in `triage` mode, or its `.tasklane.yml` says `always_ask: true` | Every ticket waits | team approver or owner |
| The ticket text changes after approval and now matches a risk rule | Scored again; asks again if needed | as above |
| A collaborator comments or asks for changes on the pull request | The developer works on it again (their words are given as feedback), then the change is re-scored, goes through CI and review again, and asks a person if it now touches a protected path | as above |

Level rules are a **floor**. The model may raise a level and never lowers it. Matching is a case-insensitive text search, so it errs on the side of asking.

An approval is bound to a **hash of the exact plan or diff** it was given for. If anything changes, the approval no longer applies and a person is asked again. Only a person pressing a button counts: **a timer, a comment, or typing "ok" never approves anything.**

## When tasklane stops and hands over

| Situation | Result |
|---|---|
| CI fails twice | The ticket moves to `failed` for a person (they can retry or close it) |
| The developer agent fails three times, or the reviewer keeps asking for changes | `failed` |
| The ticket uses more model tokens than its budget (`TASKLANE_MAX_TOKENS_PER_TICKET`) | `failed` before another model run |
| The agent changes nothing, or CI never reports | `failed` with the reason |
| Nobody answers the reporter for 14 days, or nobody approves for 30 days | Closed as stale. The timer only closes; it never approves. |

A retry never skips review: only a commit a reviewer approved can go straight to merge.

## Turning the dial

| You want | Do this |
|---|---|
| Everything on its own for safe repositories | Mode `full` on those repositories |
| Always ask a person | Mode `triage`, or `always_ask: true` in `.tasklane.yml` |
| More protected areas | Add paths in the console, or `protected_paths` in `.tasklane.yml` |
| Smaller changes only | `max_diff_lines` in `.tasklane.yml` |
| A hard stop | Mode `watch`, or **Pause all work** in the console |
| Limit cost | `TASKLANE_MAX_TOKENS_PER_TICKET`, and `TASKLANE_MAX_DEV` for parallel runs |

A repository's own `.tasklane.yml` can only tighten these settings; it can never loosen them (see the [operations guide](operations.md)).

## Why it is built this way

An autonomous coding agent is useful only if you can say exactly when it may act alone. Here that is a short list of fixed rules on the real text and the real diff, with every escalation, approval and merge written to an audit trail. The agent never holds your credentials, never pushes and never merges: tasklane does, and only the commit that was reviewed.
