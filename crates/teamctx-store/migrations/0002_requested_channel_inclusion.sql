-- Preserve connector intent independently of inherited effective visibility.
-- v1 retained only the effective flag; keep existing exclusions conservatively
-- until the connector next supplies the channel's current configuration decision.
ALTER TABLE channels ADD COLUMN requested_included INTEGER NOT NULL DEFAULT 1
 CHECK(requested_included IN (0,1));
UPDATE channels SET requested_included=is_included;
