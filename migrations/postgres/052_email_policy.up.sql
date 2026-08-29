-- Held in memory and swapped whole on refresh; nothing queries it per signup.
CREATE TABLE disposable_email_domains (
    domain TEXT PRIMARY KEY
);

-- Singleton. `fetched_at` decides staleness, `domain_count` is the baseline the
-- shrink guard rejects a truncated upstream against.
CREATE TABLE disposable_email_refresh (
    id           BOOLEAN     PRIMARY KEY DEFAULT true CHECK (id),
    fetched_at   TIMESTAMPTZ NOT NULL,
    domain_count INTEGER     NOT NULL CHECK (domain_count >= 0)
);

-- Written once at signup and kept after a list changes its mind, so churn
-- analysis can tell a burner from a later false positive.
ALTER TABLE users
    ADD COLUMN email_risk TEXT
    CHECK (email_risk IN ('disposable', 'no_mx'));

-- Back-office lists flagged accounts; the unmarked majority stays out of it.
CREATE INDEX idx_users_email_risk
    ON users(email_risk)
    WHERE email_risk IS NOT NULL AND deleted_at IS NULL;
