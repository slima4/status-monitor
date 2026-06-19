-- Process-generated secrets that must persist across restarts and survive
-- unrelated config rotation (e.g. the public unsubscribe HMAC key, which can't
-- depend on auth.fingerprint_salt or rotating that salt would void mailed
-- unsubscribe links). One row per named secret, generated on first boot.
CREATE TABLE app_secrets (
    name       TEXT PRIMARY KEY,
    secret     TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
