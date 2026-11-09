-- The time (ISO, from the source) of the newest pull-request comment already acted on.
ALTER TABLE tickets ADD COLUMN feedback_since text;
