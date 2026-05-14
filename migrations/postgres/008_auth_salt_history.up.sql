-- Rotation history for `auth.fingerprint_salt`. The startup guard refuses to
-- boot if the current configured salt isn't recorded here, so an accidental
-- salt rotation can't silently fragment anomaly-detection windows. Only the
-- SHA-256 of the salt is stored — never the salt itself.

CREATE TABLE auth_salt_history (
    salt_sha256    TEXT PRIMARY KEY,
    first_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
