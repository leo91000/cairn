-- Existing sessions remain usable, but must prove identity before account deletion.
-- OAuth sessions do not qualify: only email and verified passkey proofs do.
ALTER TABLE web_sessions ADD COLUMN authenticated_at timestamptz;
CREATE INDEX account_audit_actor ON account_audit(actor_id, id DESC);
