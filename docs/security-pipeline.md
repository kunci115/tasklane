# Security pipeline: alerts in, audit log out

tasklane is not a SIEM and does not detect or block attacks. It helps with the part after detection: an alert
becomes a ticket, an agent proposes the fix as a pull request, a person approves the exact plan and diff, and the
result is visible in the SIEM. Both directions are off until you set a secret.

```
SIEM / scanner --(signed alert)--> tasklane ticket --> triage --> person approves --> fix PR --> review --> merge
SIEM / collector <--(poll)-- tasklane audit log (hash-chained, secrets removed)
```

## Alerts in: `POST /api/v1/alerts`

Set `TASKLANE_ALERT_SECRET` (16 characters or more). Sign the exact request body with HMAC-SHA256 and send the hex
digest as `X-Tasklane-Signature: sha256=<digest>`. Without the variable the endpoint does not exist.

```bash
BODY='{"source":"wazuh","fingerprint":"rule-5710:10.0.0.9","severity":"high",
 "title":"Brute force on /login","description":"900 failed logins from 10.0.0.9 in 5 minutes",
 "repo":"your-org/your-repo","details":{"count":900}}'
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$TASKLANE_ALERT_SECRET" -hex | sed 's/^.* //')
curl -X POST https://tasklane.example/api/v1/alerts -H "X-Tasklane-Signature: sha256=$SIG" \
     -H 'Content-Type: application/json' --data "$BODY"
```

| Field | Meaning |
|---|---|
| `source` | Who sent it (letters, digits, `_ . -`, up to 40). Becomes the reporter `alert:<source>`. |
| `fingerprint` | Identifies the alert. The same `source` + `fingerprint` while its ticket is open only adds to a counter (`200`, `"status":"duplicate"`); after the ticket is finished a new ticket is made and says the alert came back. |
| `severity` | `info`, `low`, `medium`, `high`, `critical`. |
| `title`, `description`, `details` | Text for the issue (150, 8000 and 4000 characters). |
| `repo` | `owner/name` of a repository tasklane works on. It must not be in `watch` mode. |

What the alert can and can not do:

- Severity is a **floor** on the risk level (`medium` → level 2, `high` and `critical` → level 3). Triage can raise it, never lower it.
- A ticket made from an alert **always waits for a person**, whatever its level or the repository's mode.
- The alert's text goes into the issue inside a fenced block marked as data from the alert source. Anything that looks like an instruction in it is not one.
- Nothing is merged without the usual approval of the exact plan and diff.

## Audit log out: `GET /api/v1/audit`

Set `TASKLANE_SIEM_TOKEN` (16 characters or more); a collector sends it as `Authorization: Bearer <token>`.

`?after=<id>&limit=<n>&format=jsonl|cef|syslog` returns events newer than `after`, oldest first (limit 1 to 5000,
default 1000). The header `X-Tasklane-Next` is the id to ask for next time. Polling every minute with the last id
is enough for Splunk, Elastic, Wazuh or Sentinel; `cef` and `syslog` lines can go straight to a syslog input.

- Every event carries `prev` and `hash`: the hash of the event before it. A collector that keeps what it read can
  tell that a row was removed or moved afterwards (a break in `prev`). The hash covers the stored event; it proves
  continuity, not authorship, and a person with write access to the database could rebuild the chain, so keep the
  collector's copy.
- Values under names that look secret (`token`, `secret`, `password`, `authorization`, `credential`, `key`) are
  replaced with `[redacted]`, and long text is cut at 500 characters.
- What is logged: approvals and rejections, merges and refused merges, retries, stops, messages to agents, sign-ins
  of people, alerts received, and more (see the Activity page).

## Not covered

- tasklane does not read your logs or watch traffic; the SIEM, the WAF and the bot or DDoS protection do that.
- Real-time response. The path from alert to merged fix takes minutes to hours.
- Closing the alert in the SIEM after the merge (a status call back) and per-vendor connectors are planned.
