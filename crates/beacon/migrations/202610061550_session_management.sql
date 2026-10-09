-- The public identifier is independent of the bearer digest and CSRF proof.
ALTER TABLE web_sessions ADD COLUMN id text NOT NULL DEFAULT gen_random_uuid()::text;
ALTER TABLE web_sessions ADD CONSTRAINT web_sessions_id_unique UNIQUE (id);
ALTER TABLE web_sessions ADD COLUMN created_at timestamptz;
-- Existing sessions have the same seven-day lifetime; preserve their deadlines.
UPDATE web_sessions SET created_at = expires_at - interval '7 days';
ALTER TABLE web_sessions ALTER COLUMN created_at SET NOT NULL;
ALTER TABLE web_sessions ALTER COLUMN created_at SET DEFAULT now();
CREATE INDEX web_sessions_account ON web_sessions(account_id);
