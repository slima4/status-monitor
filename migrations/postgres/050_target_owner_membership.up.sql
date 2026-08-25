-- Fail the boot rather than queue every reader behind the ALTER below.
SET LOCAL lock_timeout = '5s';

-- Owners left behind by removals that ran before the constraint below existed.
UPDATE targets t
   SET owner_user_id = NULL
 WHERE owner_user_id IS NOT NULL
   AND NOT EXISTS (
       SELECT 1 FROM memberships m
        WHERE m.org_id = t.org_id AND m.user_id = t.owner_user_id);

-- Column order matches the memberships primary key, so this needs no new index.
-- The column list keeps the NOT NULL org_id out of the SET NULL. No ON UPDATE:
-- cascading one would rewrite targets.org_id, moving monitors between tenants.
ALTER TABLE targets
    ADD CONSTRAINT targets_owner_is_member_fk
    FOREIGN KEY (owner_user_id, org_id) REFERENCES memberships(user_id, org_id)
    ON DELETE SET NULL (owner_user_id);
