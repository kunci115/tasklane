-- Notes a person wants every agent to know about one repository. Only people write them
-- (never a model, never ticket text), so a note can carry the same trust as the agent instructions.
CREATE TABLE memory_notes (
  id         bigserial PRIMARY KEY,
  service_id bigint NOT NULL REFERENCES services(id) ON DELETE CASCADE,
  fact       text NOT NULL CHECK (char_length(fact) BETWEEN 1 AND 1000),
  provenance text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX memory_notes_service ON memory_notes (service_id, id DESC);
