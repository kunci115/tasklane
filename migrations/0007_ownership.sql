-- Which tasklane install may act on a repository. Two installs on one repository would both answer every
-- issue, so each repository is claimed by one install (see ownership in the orchestrator).
ALTER TABLE services
  ADD COLUMN owner_state      text NOT NULL DEFAULT 'unknown',  -- 'unknown' | 'mine' | 'foreign'
  ADD COLUMN owner_name       text,                             -- the install that owns it, when it is not this one
  ADD COLUMN owner_checked_at timestamptz;
