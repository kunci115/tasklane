-- A ticket that looks like an earlier one is proposed as a duplicate; a person confirms or rejects.
-- `duplicate_checked` keeps a rejected proposal from being made again.
ALTER TABLE tickets ADD COLUMN duplicate_of text;
ALTER TABLE tickets ADD COLUMN duplicate_checked boolean NOT NULL DEFAULT false;
