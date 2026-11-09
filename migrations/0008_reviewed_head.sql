-- The head commit a reviewer approved. A retry may only merge a head that was approved;
-- anything else goes back through CI and review.
ALTER TABLE tickets ADD COLUMN reviewed_head text;
