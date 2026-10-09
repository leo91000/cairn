CREATE TABLE mcp_grants (
    id text PRIMARY KEY,
    account_id text NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    installation_id text NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    client_id text,
    label text NOT NULL,
    scopes text[] NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX mcp_grants_account_installation ON mcp_grants(account_id, installation_id);

CREATE TABLE mcp_tokens (
    digest text PRIMARY KEY,
    grant_id text NOT NULL REFERENCES mcp_grants(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('access', 'refresh')),
    scopes text[] NOT NULL,
    used boolean NOT NULL DEFAULT false,
    expires_at timestamptz NOT NULL
);

CREATE TABLE mcp_clients (
    id text PRIMARY KEY,
    name text NOT NULL,
    redirect_uris text[] NOT NULL
);
ALTER TABLE mcp_grants ADD CONSTRAINT mcp_grants_client_id_fkey
    FOREIGN KEY (client_id) REFERENCES mcp_clients(id);

CREATE TABLE mcp_codes (
    digest text PRIMARY KEY,
    account_id text NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    installation_id text NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    client_id text NOT NULL REFERENCES mcp_clients(id),
    redirect_uri text NOT NULL,
    challenge text NOT NULL,
    scopes text[] NOT NULL,
    expires_at timestamptz NOT NULL
);
