-- Existing registrations get a full grace period from this migration.
ALTER TABLE mcp_clients ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();
CREATE INDEX mcp_clients_created ON mcp_clients(created_at);
CREATE INDEX mcp_grants_client ON mcp_grants(client_id);
CREATE INDEX mcp_grants_expiry ON mcp_grants(expires_at);
CREATE INDEX mcp_codes_client ON mcp_codes(client_id);
CREATE INDEX mcp_codes_unused_expiry ON mcp_codes(expires_at) WHERE grant_id IS NULL;
CREATE INDEX mcp_tokens_expiry ON mcp_tokens(expires_at);
