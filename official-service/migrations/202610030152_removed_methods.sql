-- Remember removed identities so sign-in cannot silently re-enable them.
ALTER TABLE sign_in_methods ADD COLUMN removed BOOLEAN NOT NULL DEFAULT false;
