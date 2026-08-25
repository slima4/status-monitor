-- One-way: the owners nulled above are not recorded anywhere to put back.
ALTER TABLE targets DROP CONSTRAINT IF EXISTS targets_owner_is_member_fk;
