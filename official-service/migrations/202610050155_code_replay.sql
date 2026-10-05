-- Keep only code digests/bindings for replay detection while a grant exists.
ALTER TABLE mcp_codes ADD COLUMN grant_id text REFERENCES mcp_grants(id) ON DELETE CASCADE;
CREATE INDEX mcp_codes_grant ON mcp_codes(grant_id);
