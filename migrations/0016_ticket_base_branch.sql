-- The branch a ticket's pull request goes to, fixed when development starts so that a later
-- change of the repository's setting cannot move a ticket that is already under way.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS base_branch text;
