-- A person can sign in to the console with a token of their own, and the session remembers who.
ALTER TABLE principals ADD COLUMN IF NOT EXISTS console_token_hash text UNIQUE;
ALTER TABLE principals ADD COLUMN IF NOT EXISTS console_token_hint text;
ALTER TABLE console_sessions ADD COLUMN IF NOT EXISTS principal_id bigint REFERENCES principals(id) ON DELETE CASCADE;
