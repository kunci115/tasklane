-- Connection settings made in the console (non-secret parts; secrets live in files under the data dir),
-- and the one-time states that protect the GitHub App creation flow.
CREATE TABLE settings (
  key        text PRIMARY KEY,
  value      jsonb NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE setup_states (
  state      text PRIMARY KEY,
  session_id text NOT NULL,
  purpose    text NOT NULL,
  expires_at timestamptz NOT NULL
);
