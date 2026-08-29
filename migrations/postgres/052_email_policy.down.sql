DROP INDEX IF EXISTS idx_users_email_risk;
ALTER TABLE users DROP COLUMN IF EXISTS email_risk;
DROP TABLE IF EXISTS disposable_email_refresh;
DROP TABLE IF EXISTS disposable_email_domains;
