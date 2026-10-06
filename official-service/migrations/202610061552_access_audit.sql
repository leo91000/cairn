CREATE TABLE account_audit (
    id bigserial PRIMARY KEY,
    account_id text REFERENCES leo_accounts(id) ON DELETE SET NULL,
    actor_id text NOT NULL,
    installation_id text,
    action text NOT NULL,
    target_id text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
-- Actor/installation/target IDs remain opaque metadata after deletion, never
-- emails, names, credentials, request bodies or installation content.
CREATE INDEX account_audit_account ON account_audit(account_id, id DESC);
CREATE INDEX account_audit_expiry ON account_audit(created_at);
