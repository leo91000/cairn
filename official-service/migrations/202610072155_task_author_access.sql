-- A re-invitation must not revive commitments made during a previous membership.
ALTER TABLE installation_members
    ADD COLUMN access_id text NOT NULL DEFAULT gen_random_uuid()::text;
