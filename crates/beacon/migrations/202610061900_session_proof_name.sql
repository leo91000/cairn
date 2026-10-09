-- Name the last independent email/passkey proof, not the OAuth/session login.
-- Preserve existing values and every historical SQLx migration checksum.
ALTER TABLE web_sessions RENAME COLUMN authenticated_at TO last_proof_at;
COMMENT ON COLUMN web_sessions.last_proof_at IS
    'Last email-code or user-verified passkey proof in this session; reusable for five minutes';
