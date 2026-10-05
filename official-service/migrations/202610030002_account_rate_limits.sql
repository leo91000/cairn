CREATE TABLE account_rate_limits (
    key TEXT PRIMARY KEY,
    requests INTEGER NOT NULL,
    resets_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX account_rate_limits_expiry ON account_rate_limits (resets_at);
