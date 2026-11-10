-- A note with no repository is for every repository.
ALTER TABLE memory_notes ALTER COLUMN service_id DROP NOT NULL;
CREATE INDEX IF NOT EXISTS memory_notes_global ON memory_notes (id DESC) WHERE service_id IS NULL;
