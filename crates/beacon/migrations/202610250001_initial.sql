CREATE FUNCTION cairn_readable_slug() RETURNS text
    LANGUAGE sql
    AS $$
    SELECT (ARRAY['bright', 'calm', 'clear', 'gentle', 'happy', 'little', 'lucky', 'merry', 'quiet', 'silver', 'soft', 'sunny', 'swift', 'warm', 'wild', 'wise'])[1 + floor(random() * 16)::int] || '-' ||
           (ARRAY['birch', 'brook', 'cedar', 'comet', 'coral', 'dawn', 'fern', 'finch', 'fox', 'maple', 'moon', 'otter', 'pine', 'robin', 'star', 'willow'])[1 + floor(random() * 16)::int] || '-' ||
           substr(replace(gen_random_uuid()::text, '-', ''), 25, 8)
$$;

CREATE TABLE account_audit (
    id bigint NOT NULL,
    account_id text,
    actor_id text NOT NULL,
    installation_id text,
    action text NOT NULL,
    target_id text,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL
);

CREATE SEQUENCE account_audit_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE account_audit_id_seq OWNED BY account_audit.id;

CREATE TABLE account_rate_limits (
    key text NOT NULL,
    requests integer NOT NULL,
    resets_at timestamp with time zone NOT NULL
);

CREATE TABLE cairn_accounts (
    id text NOT NULL,
    email text NOT NULL,
    verified_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE client_performance (
    account_id text NOT NULL,
    window_id text NOT NULL,
    sequence bigint NOT NULL,
    report jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT client_performance_sequence_check CHECK ((sequence > 0))
);

CREATE TABLE email_code_challenges (
    challenge text NOT NULL,
    code_challenge text NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    CONSTRAINT email_code_challenges_attempts_check CHECK (((attempts >= 0) AND (attempts <= 5)))
);

CREATE TABLE email_codes (
    challenge text NOT NULL,
    email text NOT NULL,
    code_digest text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    CONSTRAINT email_code_attempts CHECK (((attempts >= 0) AND (attempts <= 50)))
);

CREATE TABLE installation_claim_codes (
    digest text NOT NULL,
    account_id text NOT NULL,
    expires_at timestamp with time zone NOT NULL
);

CREATE TABLE installation_device_claims (
    device_digest text NOT NULL,
    user_digest text NOT NULL,
    installation_id text NOT NULL,
    approved_by text,
    expires_at timestamp with time zone NOT NULL,
    confirmation_digest text,
    reviewed_by text,
    installation_name text NOT NULL,
    recovering boolean NOT NULL
);

CREATE TABLE installation_invitations (
    id text NOT NULL,
    installation_id text NOT NULL,
    email text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '7 days'::interval) NOT NULL
);

CREATE TABLE installation_members (
    installation_id text NOT NULL,
    account_id text NOT NULL,
    joined_at timestamp with time zone DEFAULT now() NOT NULL,
    access_id text DEFAULT (gen_random_uuid())::text NOT NULL
);

CREATE TABLE installations (
    id text NOT NULL,
    owner_id text,
    name text NOT NULL,
    token_digest text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    recovery_digest text,
    update_required boolean DEFAULT false NOT NULL,
    slug text DEFAULT cairn_readable_slug() NOT NULL
);

CREATE TABLE mcp_clients (
    id text NOT NULL,
    name text NOT NULL,
    redirect_uris text[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE mcp_codes (
    digest text NOT NULL,
    account_id text NOT NULL,
    installation_id text NOT NULL,
    client_id text NOT NULL,
    redirect_uri text NOT NULL,
    challenge text NOT NULL,
    scopes text[] NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    grant_id text
);

CREATE TABLE mcp_grants (
    id text NOT NULL,
    account_id text NOT NULL,
    installation_id text NOT NULL,
    client_id text,
    label text NOT NULL,
    scopes text[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL
);

CREATE TABLE mcp_tokens (
    digest text NOT NULL,
    grant_id text NOT NULL,
    kind text NOT NULL,
    scopes text[] NOT NULL,
    used boolean DEFAULT false NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    CONSTRAINT mcp_tokens_kind_check CHECK ((kind = ANY (ARRAY['access'::text, 'refresh'::text])))
);

CREATE TABLE native_oauth_handovers (
    id text NOT NULL,
    provider text NOT NULL,
    secret_digest text NOT NULL,
    launcher_digest text,
    browser_token text,
    authorization_url text NOT NULL,
    link_account text,
    session_digest text NOT NULL,
    ready_account text,
    method_subject text,
    expires_at timestamp with time zone DEFAULT (now() + '00:05:00'::interval) NOT NULL,
    pending_identity text,
    confirmation_digest text,
    confirmation_browser_digest text,
    CONSTRAINT native_oauth_handovers_provider_check CHECK ((provider = 'github'::text))
);

CREATE TABLE notification_devices (
    id text NOT NULL,
    account_id text NOT NULL,
    endpoint text NOT NULL,
    p256dh text NOT NULL,
    auth text NOT NULL
);

CREATE TABLE sign_in_challenges (
    id text NOT NULL,
    kind text NOT NULL,
    browser_digest text NOT NULL,
    account_id text,
    session_digest text,
    state text NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '00:05:00'::interval) NOT NULL
);

CREATE TABLE sign_in_methods (
    id text NOT NULL,
    account_id text NOT NULL,
    kind text NOT NULL,
    subject text NOT NULL,
    label text NOT NULL,
    credential text,
    removed boolean DEFAULT false NOT NULL,
    CONSTRAINT sign_in_methods_kind_check CHECK ((kind = ANY (ARRAY['email'::text, 'google'::text, 'github'::text, 'passkey'::text])))
);

CREATE TABLE web_sessions (
    digest text NOT NULL,
    account_id text NOT NULL,
    csrf text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    id text DEFAULT (gen_random_uuid())::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    device text DEFAULT 'Unknown device'::text NOT NULL,
    last_proof_at timestamp with time zone,
    CONSTRAINT web_sessions_device_length CHECK ((length(device) <= 256))
);

COMMENT ON COLUMN web_sessions.last_proof_at IS 'Last email-code or user-verified passkey proof in this session; reusable for five minutes';

ALTER TABLE ONLY account_audit ALTER COLUMN id SET DEFAULT nextval('account_audit_id_seq'::regclass);

ALTER TABLE ONLY account_audit
    ADD CONSTRAINT account_audit_pkey PRIMARY KEY (id);

ALTER TABLE ONLY account_rate_limits
    ADD CONSTRAINT account_rate_limits_pkey PRIMARY KEY (key);

ALTER TABLE ONLY cairn_accounts
    ADD CONSTRAINT cairn_accounts_email_key UNIQUE (email);

ALTER TABLE ONLY cairn_accounts
    ADD CONSTRAINT cairn_accounts_pkey PRIMARY KEY (id);

ALTER TABLE ONLY client_performance
    ADD CONSTRAINT client_performance_pkey PRIMARY KEY (account_id, window_id);

ALTER TABLE ONLY email_code_challenges
    ADD CONSTRAINT email_code_challenges_pkey PRIMARY KEY (challenge);

ALTER TABLE ONLY email_codes
    ADD CONSTRAINT email_codes_pkey PRIMARY KEY (challenge);

ALTER TABLE ONLY installation_claim_codes
    ADD CONSTRAINT installation_claim_codes_pkey PRIMARY KEY (digest);

ALTER TABLE ONLY installation_device_claims
    ADD CONSTRAINT installation_device_claims_installation_id_key UNIQUE (installation_id);

ALTER TABLE ONLY installation_device_claims
    ADD CONSTRAINT installation_device_claims_pkey PRIMARY KEY (device_digest);

ALTER TABLE ONLY installation_device_claims
    ADD CONSTRAINT installation_device_claims_user_digest_key UNIQUE (user_digest);

ALTER TABLE ONLY installation_invitations
    ADD CONSTRAINT installation_invitations_installation_id_email_key UNIQUE (installation_id, email);

ALTER TABLE ONLY installation_invitations
    ADD CONSTRAINT installation_invitations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY installation_members
    ADD CONSTRAINT installation_members_pkey PRIMARY KEY (installation_id, account_id);

ALTER TABLE ONLY installations
    ADD CONSTRAINT installations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY mcp_clients
    ADD CONSTRAINT mcp_clients_pkey PRIMARY KEY (id);

ALTER TABLE ONLY mcp_codes
    ADD CONSTRAINT mcp_codes_pkey PRIMARY KEY (digest);

ALTER TABLE ONLY mcp_grants
    ADD CONSTRAINT mcp_grants_pkey PRIMARY KEY (id);

ALTER TABLE ONLY mcp_tokens
    ADD CONSTRAINT mcp_tokens_pkey PRIMARY KEY (digest);

ALTER TABLE ONLY native_oauth_handovers
    ADD CONSTRAINT native_oauth_handovers_pkey PRIMARY KEY (id);

ALTER TABLE ONLY notification_devices
    ADD CONSTRAINT notification_devices_endpoint_key UNIQUE (endpoint);

ALTER TABLE ONLY notification_devices
    ADD CONSTRAINT notification_devices_pkey PRIMARY KEY (id);

ALTER TABLE ONLY sign_in_challenges
    ADD CONSTRAINT sign_in_challenges_pkey PRIMARY KEY (id);

ALTER TABLE ONLY sign_in_methods
    ADD CONSTRAINT sign_in_methods_kind_subject_key UNIQUE (kind, subject);

ALTER TABLE ONLY sign_in_methods
    ADD CONSTRAINT sign_in_methods_pkey PRIMARY KEY (id);

ALTER TABLE ONLY web_sessions
    ADD CONSTRAINT web_sessions_id_unique UNIQUE (id);

ALTER TABLE ONLY web_sessions
    ADD CONSTRAINT web_sessions_pkey PRIMARY KEY (digest);

CREATE INDEX account_audit_account ON account_audit USING btree (account_id, id DESC);

CREATE INDEX account_audit_actor ON account_audit USING btree (actor_id, id DESC);

CREATE INDEX account_audit_expiry ON account_audit USING btree (created_at);

CREATE INDEX account_rate_limits_expiry ON account_rate_limits USING btree (resets_at);

CREATE INDEX client_performance_account_recent ON client_performance USING btree (account_id, updated_at DESC);

CREATE INDEX client_performance_expiry ON client_performance USING btree (updated_at);

CREATE INDEX email_code_challenges_code ON email_code_challenges USING btree (code_challenge);

CREATE INDEX email_codes_email ON email_codes USING btree (email);

CREATE INDEX email_codes_expiry ON email_codes USING btree (expires_at);

CREATE INDEX installation_invitations_email ON installation_invitations USING btree (email);

CREATE INDEX installation_members_account ON installation_members USING btree (account_id);

CREATE INDEX installations_owner ON installations USING btree (owner_id);

CREATE UNIQUE INDEX installations_slug ON installations USING btree (slug);

CREATE INDEX mcp_clients_created ON mcp_clients USING btree (created_at);

CREATE INDEX mcp_codes_client ON mcp_codes USING btree (client_id);

CREATE INDEX mcp_codes_grant ON mcp_codes USING btree (grant_id);

CREATE INDEX mcp_codes_unused_expiry ON mcp_codes USING btree (expires_at) WHERE (grant_id IS NULL);

CREATE INDEX mcp_grants_account_installation ON mcp_grants USING btree (account_id, installation_id);

CREATE INDEX mcp_grants_client ON mcp_grants USING btree (client_id);

CREATE INDEX mcp_grants_expiry ON mcp_grants USING btree (expires_at);

CREATE INDEX mcp_tokens_expiry ON mcp_tokens USING btree (expires_at);

CREATE INDEX native_oauth_expiry ON native_oauth_handovers USING btree (expires_at);

CREATE INDEX notification_devices_account ON notification_devices USING btree (account_id);

CREATE INDEX sign_in_challenges_expiry ON sign_in_challenges USING btree (expires_at);

CREATE INDEX sign_in_methods_account ON sign_in_methods USING btree (account_id);

CREATE INDEX web_sessions_account ON web_sessions USING btree (account_id);

CREATE INDEX web_sessions_expiry ON web_sessions USING btree (expires_at);

ALTER TABLE ONLY client_performance
    ADD CONSTRAINT client_performance_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY email_code_challenges
    ADD CONSTRAINT email_code_challenges_code_challenge_fkey FOREIGN KEY (code_challenge) REFERENCES email_codes(challenge) ON DELETE CASCADE;

ALTER TABLE ONLY installation_claim_codes
    ADD CONSTRAINT installation_claim_codes_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY installation_device_claims
    ADD CONSTRAINT installation_device_claims_approved_by_fkey FOREIGN KEY (approved_by) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY installation_device_claims
    ADD CONSTRAINT installation_device_claims_reviewed_by_fkey FOREIGN KEY (reviewed_by) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY installation_invitations
    ADD CONSTRAINT installation_invitations_installation_id_fkey FOREIGN KEY (installation_id) REFERENCES installations(id) ON DELETE CASCADE;

ALTER TABLE ONLY installation_members
    ADD CONSTRAINT installation_members_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY installation_members
    ADD CONSTRAINT installation_members_installation_id_fkey FOREIGN KEY (installation_id) REFERENCES installations(id) ON DELETE CASCADE;

ALTER TABLE ONLY installations
    ADD CONSTRAINT installations_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES cairn_accounts(id) ON DELETE SET NULL;

ALTER TABLE ONLY mcp_codes
    ADD CONSTRAINT mcp_codes_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY mcp_codes
    ADD CONSTRAINT mcp_codes_client_id_fkey FOREIGN KEY (client_id) REFERENCES mcp_clients(id);

ALTER TABLE ONLY mcp_codes
    ADD CONSTRAINT mcp_codes_grant_id_fkey FOREIGN KEY (grant_id) REFERENCES mcp_grants(id) ON DELETE CASCADE;

ALTER TABLE ONLY mcp_codes
    ADD CONSTRAINT mcp_codes_installation_id_fkey FOREIGN KEY (installation_id) REFERENCES installations(id) ON DELETE CASCADE;

ALTER TABLE ONLY mcp_grants
    ADD CONSTRAINT mcp_grants_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY mcp_grants
    ADD CONSTRAINT mcp_grants_client_id_fkey FOREIGN KEY (client_id) REFERENCES mcp_clients(id);

ALTER TABLE ONLY mcp_grants
    ADD CONSTRAINT mcp_grants_installation_id_fkey FOREIGN KEY (installation_id) REFERENCES installations(id) ON DELETE CASCADE;

ALTER TABLE ONLY mcp_tokens
    ADD CONSTRAINT mcp_tokens_grant_id_fkey FOREIGN KEY (grant_id) REFERENCES mcp_grants(id) ON DELETE CASCADE;

ALTER TABLE ONLY native_oauth_handovers
    ADD CONSTRAINT native_oauth_handovers_link_account_fkey FOREIGN KEY (link_account) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY native_oauth_handovers
    ADD CONSTRAINT native_oauth_handovers_ready_account_fkey FOREIGN KEY (ready_account) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY notification_devices
    ADD CONSTRAINT notification_devices_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY sign_in_challenges
    ADD CONSTRAINT sign_in_challenges_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY sign_in_methods
    ADD CONSTRAINT sign_in_methods_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;

ALTER TABLE ONLY web_sessions
    ADD CONSTRAINT web_sessions_account_id_fkey FOREIGN KEY (account_id) REFERENCES cairn_accounts(id) ON DELETE CASCADE;
