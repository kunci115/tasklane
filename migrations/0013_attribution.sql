-- Who gets credit for a change tasklane makes. The commit is always authored by tasklane; a person is
-- named in trailers (requested by, approved by) and is credited as a co-author, which makes the commit
-- count in their GitHub contribution graph, only when they chose that themselves.
ALTER TABLE principals
  ADD COLUMN github_login    text,
  ADD COLUMN github_id       bigint,
  ADD COLUMN credit_coauthor boolean NOT NULL DEFAULT false;
