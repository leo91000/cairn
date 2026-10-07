-- Older in-flight handovers were ready without browser consent. Invalidate them
-- when upgrading; they cannot be grandfathered into the protected exchange.
DELETE FROM native_oauth_handovers;

ALTER TABLE native_oauth_handovers
    ADD COLUMN pending_identity TEXT,
    ADD COLUMN confirmation_digest TEXT,
    ADD COLUMN confirmation_browser_digest TEXT;
