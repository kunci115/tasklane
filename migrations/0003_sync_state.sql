-- Small key/value store for the sync job (cursors).
CREATE TABLE sync_state (
  key   text PRIMARY KEY,
  value text NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);
