CREATE TABLE installation_device_claims (
    device_digest text PRIMARY KEY,
    user_digest text NOT NULL UNIQUE,
    installation_id text NOT NULL UNIQUE REFERENCES installations(id) ON DELETE CASCADE,
    approved_by text REFERENCES leo_accounts(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);
