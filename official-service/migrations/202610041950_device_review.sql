-- Device-code entry reviews the request; only an explicit confirmation approves it.
ALTER TABLE installation_device_claims ADD COLUMN confirmation_digest text;
ALTER TABLE installation_device_claims ADD COLUMN reviewed_by text REFERENCES leo_accounts(id) ON DELETE CASCADE;

-- A fresh device claim reserves an ID only in the expiring challenge. Persist
-- the installation when the machine collects an approved claim, never on start.
ALTER TABLE installation_device_claims DROP CONSTRAINT installation_device_claims_installation_id_fkey;
ALTER TABLE installation_device_claims ADD COLUMN installation_name text;
ALTER TABLE installation_device_claims ADD COLUMN recovering boolean NOT NULL DEFAULT true;
UPDATE installation_device_claims AS claim
SET installation_name = installations.name,
    recovering = installations.recovery_digest IS NOT NULL
FROM installations WHERE installations.id = claim.installation_id;
-- These rows were created by the old device-start handler, with a random token
-- never delivered to a machine. Genuine detached records have a recovery proof.
DELETE FROM installations USING installation_device_claims AS claim
WHERE installations.id = claim.installation_id
  AND installations.owner_id IS NULL AND NOT claim.recovering;
ALTER TABLE installation_device_claims ALTER COLUMN installation_name SET NOT NULL;
ALTER TABLE installation_device_claims ALTER COLUMN recovering DROP DEFAULT;
