-- Title as reported through the gateway (for duplicate checks) and the plan
-- hash an approval request was last announced for (so each is announced once).
ALTER TABLE tickets ADD COLUMN title text;
ALTER TABLE tickets ADD COLUMN approval_notified_hash text;
CREATE INDEX tickets_reporter_idx ON tickets (reporter_ref, created_at);
