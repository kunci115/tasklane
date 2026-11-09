# 02. Database schema (Postgres)

Status: draft 1. Names may change before implementation. All timestamps are `timestamptz`. All tables have `created_at`. Secrets are never stored here (the broker keeps credentials in its own store).

```sql
-- Services: one row per thing agents can work on (a repo, a device group, ...).
CREATE TABLE services (
  id            bigserial PRIMARY KEY,
  kind          text NOT NULL,                 -- 'repo' | 'device_group'
  source        text NOT NULL,                 -- plug-in name, e.g. 'github'
  external_id   text NOT NULL,                 -- e.g. 'owner/name'
  mode          text NOT NULL DEFAULT 'watch', -- 'watch' | 'triage' | 'full'
  facts         jsonb NOT NULL DEFAULT '{}',   -- language, test command, environments
  overrides     jsonb NOT NULL DEFAULT '{}',   -- from an optional file in the repo
  agent_profile jsonb NOT NULL DEFAULT '{}',   -- prompt, allowed actions, token budget
  archived      boolean NOT NULL DEFAULT false,
  created_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (source, external_id)
);

-- Tickets: a mirror of the source's item plus our own state.
CREATE TABLE tickets (
  id            bigserial PRIMARY KEY,
  service_id    bigint NOT NULL REFERENCES services(id),
  external_ref  text NOT NULL,                 -- issue number / key on the source
  state         text NOT NULL DEFAULT 'new',   -- see 01-flow-and-state-machine.md
  level         smallint,                      -- 1, 2, 3 (null until escalation)
  level_reasons jsonb NOT NULL DEFAULT '[]',
  plan_hash     text,                          -- hash of the approved plan
  reporter_ref  text,                          -- opaque id from the gateway token
  ci_runs       smallint NOT NULL DEFAULT 0,
  dev_attempts  smallint NOT NULL DEFAULT 0,
  tokens_used   bigint NOT NULL DEFAULT 0,
  head_sha      text,                          -- reviewed head, set by reviewer
  created_at    timestamptz NOT NULL DEFAULT now(),
  updated_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (service_id, external_ref)
);
CREATE INDEX tickets_state_idx ON tickets (state);

-- Jobs: the queue. One row per unit of work.
CREATE TABLE jobs (
  id          bigserial PRIMARY KEY,
  ticket_id   bigint NOT NULL REFERENCES tickets(id),
  kind        text NOT NULL,                   -- 'triage'|'develop'|'rescore'|'pr'|'review'|'merge'|'ask'
  status      text NOT NULL DEFAULT 'queued',  -- queued|running|done|failed|stopped
  priority    smallint NOT NULL DEFAULT 0,
  run_after   timestamptz NOT NULL DEFAULT now(),
  lease_owner text,
  lease_until timestamptz,
  fence       bigint NOT NULL DEFAULT 0,
  attempts    smallint NOT NULL DEFAULT 0,
  checkpoint  jsonb NOT NULL DEFAULT '{}',
  created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX jobs_claim_idx ON jobs (priority DESC, run_after) WHERE status = 'queued';
CREATE INDEX jobs_lease_idx ON jobs (lease_until) WHERE status = 'running';

-- Runs: one row per agent or runner execution (for cost and audit).
CREATE TABLE runs (
  id         bigserial PRIMARY KEY,
  job_id     bigint NOT NULL REFERENCES jobs(id),
  actor      text NOT NULL,                    -- 'triage'|'dev'|'reviewer'|'pr_runner'|'merge_runner'
  engine     text,                             -- plug-in name, e.g. 'claude'
  started_at timestamptz NOT NULL DEFAULT now(),
  ended_at   timestamptz,
  outcome    text,                             -- ok|error|timeout|cancelled
  usage      jsonb NOT NULL DEFAULT '{}'       -- tokens, wall time
);

-- Approvals: who approved what, bound to the plan.
CREATE TABLE approvals (
  id          bigserial PRIMARY KEY,
  ticket_id   bigint NOT NULL REFERENCES tickets(id),
  level       smallint NOT NULL,
  plan_hash   text NOT NULL,
  approver    text NOT NULL,                   -- user ref with a role
  decision    text NOT NULL,                   -- 'approve' | 'reject'
  via         text NOT NULL,                   -- 'console'|'source'|'chat'
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- Users and roles (who may approve). Kept small on purpose.
CREATE TABLE principals (
  id        bigserial PRIMARY KEY,
  channel   text NOT NULL,                     -- 'github'|'telegram'|'console'
  ref       text NOT NULL,                     -- login or numeric id on that channel
  role      text NOT NULL,                     -- 'member'|'team_approver'|'owner'
  UNIQUE (channel, ref)
);

-- Chat groups registered to a service.
CREATE TABLE chat_groups (
  id           bigserial PRIMARY KEY,
  channel      text NOT NULL,
  group_ref    text NOT NULL,
  service_id   bigint NOT NULL REFERENCES services(id),
  daily_cap    integer NOT NULL DEFAULT 50,
  UNIQUE (channel, group_ref)
);

-- Devices and the broker's action table.
CREATE TABLE devices (
  id          bigserial PRIMARY KEY,
  service_id  bigint NOT NULL REFERENCES services(id),
  adapter     text NOT NULL,                   -- plug-in name
  address     text NOT NULL,                   -- host:port, checked against the allowlist
  meta        jsonb NOT NULL DEFAULT '{}'
);
CREATE TABLE device_actions (
  id          bigserial PRIMARY KEY,
  adapter     text NOT NULL,
  name        text NOT NULL,                   -- e.g. 'printer.clear_queue'
  level       smallint NOT NULL,               -- fixed by the table, not by the agent
  inverse     text,                            -- required for level 2
  params_schema jsonb NOT NULL,
  min_interval interval NOT NULL DEFAULT '5 minutes',
  allowed_hours text,                          -- optional time window
  UNIQUE (adapter, name)
);

-- Audit log: append-only. Aggregate poll results; log state changes and every action.
CREATE TABLE audit_events (
  id         bigserial PRIMARY KEY,
  at         timestamptz NOT NULL DEFAULT now(),
  actor      text NOT NULL,
  action     text NOT NULL,
  ticket_id  bigint,
  device_id  bigint,
  detail     jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_at_idx ON audit_events (at);
```

## Notes

- **Memory** is a plug-in with its own storage (built-in table in stage 1). It is not part of this core schema.
- `audit_events` has no UPDATE or DELETE grants for the application role; retention is done by a separate maintenance job with a documented window.
- `tickets.state` and `jobs.status` are text on purpose (easy to extend); a check constraint or enum can be added once states settle.
- Duplicate detection uses the `pg_trgm` extension on ticket titles and bodies; the model may propose a link, never close a ticket.
- A `UNIQUE (service_id, external_ref)` guarantees that one source item is one ticket, even if the gateway and the sync job both see it.
