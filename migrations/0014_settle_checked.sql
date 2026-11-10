-- When the sync last asked whether a merged ticket's change reached the default branch.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS settle_checked_at timestamptz;
