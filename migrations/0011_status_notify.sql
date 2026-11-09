-- Where to tell the person who reported a ticket how it ended ('<channel>:<group>'), and the
-- state last announced there, so each outcome is announced once.
ALTER TABLE tickets ADD COLUMN notify_ref text;
ALTER TABLE tickets ADD COLUMN notified_state text;
