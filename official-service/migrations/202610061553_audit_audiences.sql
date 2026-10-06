-- Audit audiences are opaque metadata, like actors and targets. A foreign key
-- would lock another owner's account while recording a member departure,
-- inverting the account -> installation lock order of account deletion.
ALTER TABLE account_audit DROP CONSTRAINT account_audit_account_id_fkey;
-- Account deletion clears its audience explicitly in the same transaction.
