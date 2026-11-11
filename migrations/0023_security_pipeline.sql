-- Security pipeline: alerts from a SIEM or scanner become tickets, and the audit log can be read by a SIEM.

-- A ticket that must wait for a person whatever the triage says, and a risk level it can not be below.
ALTER TABLE tickets ADD COLUMN require_approval boolean NOT NULL DEFAULT false;
ALTER TABLE tickets ADD COLUMN min_level smallint NOT NULL DEFAULT 1 CHECK (min_level BETWEEN 1 AND 3);

-- One row per alert (source + fingerprint): a repeat adds to the count instead of making another ticket.
CREATE TABLE alerts (
  id          bigserial PRIMARY KEY,
  source      text NOT NULL,
  fingerprint text NOT NULL,
  severity    text NOT NULL,
  ticket_id   bigint REFERENCES tickets(id) ON DELETE SET NULL,
  count       integer NOT NULL DEFAULT 1,
  first_seen  timestamptz NOT NULL DEFAULT now(),
  last_seen   timestamptz NOT NULL DEFAULT now(),
  UNIQUE (source, fingerprint)
);

-- The chain over the audit log: each entry holds a hash of the one before it, so a SIEM that keeps what it
-- read can see rows that were removed or reordered afterwards.
CREATE TABLE audit_chain (
  id   bigint PRIMARY KEY,
  prev text NOT NULL,
  hash text NOT NULL
);
