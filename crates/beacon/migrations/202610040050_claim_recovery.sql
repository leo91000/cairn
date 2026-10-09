-- Remember the private proof used to begin recovery separately from the live
-- tunnel credential. A lost successful response must not strand that file.
-- This digest can only start a device claim on an unowned installation.
ALTER TABLE installations ADD COLUMN recovery_digest TEXT;
