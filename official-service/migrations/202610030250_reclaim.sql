-- Preserve both the installation and its machine-possession proof after detach
-- or owner deletion. The credential no longer authenticates a tunnel without
-- an owner; a successful reclaim rotates it.
ALTER TABLE installations DROP CONSTRAINT installations_owner_id_fkey;
ALTER TABLE installations ALTER COLUMN owner_id DROP NOT NULL;
ALTER TABLE installations ADD CONSTRAINT installations_owner_id_fkey
    FOREIGN KEY (owner_id) REFERENCES leo_accounts(id) ON DELETE SET NULL;
