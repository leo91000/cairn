-- Separate each browser's guess budget while retaining the shared code ceiling.
ALTER TABLE email_code_challenges
ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5);

-- Preserve failures on challenges already issued, without reopening their budget.
UPDATE email_code_challenges c
SET attempts = LEAST(e.attempts, 5)
FROM email_codes e
WHERE c.code_challenge = e.challenge;

ALTER TABLE email_codes ADD CONSTRAINT email_code_attempts CHECK (attempts BETWEEN 0 AND 50);
