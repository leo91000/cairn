CREATE TABLE client_performance (
    account_id TEXT NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    window_id TEXT NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    report JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, window_id)
);
CREATE INDEX client_performance_expiry ON client_performance(updated_at);
CREATE INDEX client_performance_account_recent ON client_performance(account_id, updated_at DESC);
