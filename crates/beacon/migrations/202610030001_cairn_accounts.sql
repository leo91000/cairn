CREATE TABLE cairn_accounts (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    verified_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE email_codes (
    challenge TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    code_digest TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX email_codes_email ON email_codes (email);
CREATE INDEX email_codes_expiry ON email_codes (expires_at);

CREATE TABLE web_sessions (
    digest TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES cairn_accounts (id) ON DELETE CASCADE,
    csrf TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX web_sessions_expiry ON web_sessions (expires_at);
