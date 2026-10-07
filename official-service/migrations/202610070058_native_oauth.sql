-- Short-lived OAuth handovers contain no session or provider access tokens.
CREATE TABLE native_oauth_handovers (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL CHECK (provider = 'github'),
    secret_digest TEXT NOT NULL,
    launcher_digest TEXT,
    browser_token TEXT,
    authorization_url TEXT NOT NULL,
    link_account TEXT REFERENCES leo_accounts(id) ON DELETE CASCADE,
    session_digest TEXT NOT NULL,
    ready_account TEXT REFERENCES leo_accounts(id) ON DELETE CASCADE,
    method_subject TEXT,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '5 minutes'
);
CREATE INDEX native_oauth_expiry ON native_oauth_handovers(expires_at);
