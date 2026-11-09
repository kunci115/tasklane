-- What the approver sees (the plan and the diff), the head that was last pushed,
-- and server-side console sessions.
ALTER TABLE tickets ADD COLUMN plan_text text;
ALTER TABLE tickets ADD COLUMN diff_text text;
ALTER TABLE tickets ADD COLUMN pushed_head text;
CREATE TABLE console_sessions (
  id         text PRIMARY KEY,
  csrf       text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz NOT NULL
);
CREATE INDEX console_sessions_expiry_idx ON console_sessions (expires_at);
