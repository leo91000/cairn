CREATE TABLE installation_members (
    installation_id text NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    account_id text NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    joined_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (installation_id, account_id)
);
CREATE INDEX installation_members_account ON installation_members(account_id);

CREATE TABLE installation_invitations (
    id text PRIMARY KEY,
    installation_id text NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    email text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '7 days',
    UNIQUE (installation_id, email)
);
CREATE INDEX installation_invitations_email ON installation_invitations(email);
