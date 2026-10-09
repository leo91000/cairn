-- Challenges issued to different browsers share one mailed code, attempt budget
-- and deadline. Deleting/consuming the code removes every challenge.
CREATE TABLE email_code_challenges (
    challenge TEXT PRIMARY KEY,
    code_challenge TEXT NOT NULL REFERENCES email_codes (challenge) ON DELETE CASCADE
);
CREATE INDEX email_code_challenges_code ON email_code_challenges (code_challenge);

-- Codes issued before this migration remain usable with their original challenge.
INSERT INTO email_code_challenges (challenge, code_challenge)
SELECT challenge, challenge FROM email_codes;
