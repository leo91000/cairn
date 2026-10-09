CREATE TABLE installation_claim_codes (
    digest text PRIMARY KEY,
    account_id text NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);

CREATE TABLE installations (
    id text PRIMARY KEY,
    owner_id text NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    name text NOT NULL,
    token_digest text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX installations_owner ON installations(owner_id);
