-- Where a ticket's work lives on the ticket source.
ALTER TABLE tickets ADD COLUMN branch text;
ALTER TABLE tickets ADD COLUMN pr_ref text;
ALTER TABLE tickets ADD COLUMN last_error text;
