-- Documents the owner points the agents at (an internal wiki page, an API's description, a runbook).
-- The daemon fetches them, never the agent; a header secret, if any, lives in the secrets directory
-- as ref_<id>. Everything fetched is passed to the agents as data.
CREATE TABLE reference_sources (
  id           bigserial PRIMARY KEY,
  name         text NOT NULL CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9 ._-]{0,39}$'),
  url          text NOT NULL CHECK (char_length(url) <= 500),
  header_name  text,
  has_secret   boolean NOT NULL DEFAULT false,
  service_id   bigint REFERENCES services(id) ON DELETE CASCADE,   -- NULL: every repository
  enabled      boolean NOT NULL DEFAULT true,
  max_bytes    integer NOT NULL DEFAULT 100000 CHECK (max_bytes BETWEEN 1000 AND 200000),
  ttl_secs     integer NOT NULL DEFAULT 600 CHECK (ttl_secs BETWEEN 60 AND 86400),
  last_status  text NOT NULL DEFAULT '',
  last_fetched timestamptz,
  last_text    text NOT NULL DEFAULT '',
  created_at   timestamptz NOT NULL DEFAULT now()
);
