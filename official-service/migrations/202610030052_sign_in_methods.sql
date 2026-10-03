CREATE TABLE sign_in_methods (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES leo_accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('email', 'google', 'github', 'passkey')),
    subject TEXT NOT NULL,
    label TEXT NOT NULL,
    credential TEXT,
    UNIQUE (kind, subject)
);
CREATE INDEX sign_in_methods_account ON sign_in_methods(account_id);
INSERT INTO sign_in_methods (id, account_id, kind, subject, label)
SELECT id, id, 'email', email, email FROM leo_accounts;

CREATE TABLE sign_in_challenges (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    browser_digest TEXT NOT NULL,
    account_id TEXT REFERENCES leo_accounts(id) ON DELETE CASCADE,
    session_digest TEXT,
    state TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '5 minutes'
);
CREATE INDEX sign_in_challenges_expiry ON sign_in_challenges(expires_at);
