-- Tools the owner lets the agents use while they work: read-only GETs below a fixed address, served by a
-- broker next to the agent for one run. A credential header, if any, is a file tool_<id> in the secrets
-- directory. Only the owner writes this table (never a ticket, a comment or a repository file).
CREATE TABLE agent_tools (
  id           bigserial PRIMARY KEY,
  name         text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_]{0,30}$'),
  description  text NOT NULL DEFAULT '' CHECK (char_length(description) <= 200),
  base_url     text NOT NULL CHECK (char_length(base_url) <= 500),
  header_name  text,
  has_secret   boolean NOT NULL DEFAULT false,
  service_id   bigint REFERENCES services(id) ON DELETE CASCADE,   -- NULL: every repository
  roles        text[] NOT NULL DEFAULT '{triage,reviewer}',
  enabled      boolean NOT NULL DEFAULT true,
  max_calls    integer NOT NULL DEFAULT 20 CHECK (max_calls BETWEEN 1 AND 100),
  calls_total  bigint NOT NULL DEFAULT 0,
  created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX agent_tools_name_scope ON agent_tools (name, coalesce(service_id, 0));
