# Privacy Policy

**Last updated:** 2026-05-15

This Privacy Policy explains how the uptimepage service ("we", "us")
collects and processes personal data. It is intended to satisfy our
obligations under the EU General Data Protection Regulation (GDPR) and
similar laws.

## 1. Data Controller

status-monitor-inc is the data controller for personal data processed via
the Service.

**Contact:** slima4.u8@gmail.com
**For data-subject requests:** slima4.u8@gmail.com (see §10)

We do not have a designated Data Protection Officer as we do not meet
the thresholds under GDPR Article 37.

## 2. What Data We Collect

We collect data in three ways:

**You provide:**
- Email address (via GitHub OAuth)
- Display name (via GitHub OAuth)
- Organisation names, slugs, branding (display name, about text, logo)
- Target configurations (URLs, intervals, headers, optional credentials)
- Status-page customisation (incident narration, maintenance windows)

**We generate automatically:**
- Session identifiers (random)
- API tokens (you create; we store hashed)
- Check results (technical metrics: status codes, latencies, error codes)
- Login attempts (success/failure, method, hashed IP, hashed user agent)
- Audit events (organisation membership changes, target changes)

**We collect via your browser:**
- Session cookie (`_sm_session`) — necessary for authentication
- IP address (hashed before storage; never stored raw)

We do **not** use third-party analytics (no Google Analytics, no
Plausible, no Mixpanel, no tracking pixels).

## 3. Why We Process This Data

| Data | Purpose | Lawful basis (GDPR Art. 6) |
|---|---|---|
| Email, display name, OAuth identity | Provide authentication | Contract |
| Targets, check results | Provide monitoring service | Contract |
| Sessions, API tokens | Authenticate API requests | Contract |
| Hashed IP, login attempts | Detect security threats | Legitimate interest |
| Audit log | Compliance and accountability | Legitimate interest |

We do not engage in automated decision-making with significant effects on
you (no profiling, no scoring).

## 4. How Long We Keep It

| Category | Retention |
|---|---|
| Account data (email, OAuth) | Until account deletion |
| Sessions | 90 days maximum |
| API tokens | Until you revoke them |
| Check results | 90 days (configurable on paid plans, not currently available) |
| Login attempts | 180 days |
| Audit log | 2 years |
| Quota events | 90 days |
| Server access logs | 30 days |
| Application error logs | 30 days |

Deleted accounts are recoverable for 30 days, after which data is
permanently purged.

## 5. Who We Share It With

We use these third-party processors:

| Processor | Purpose | Location | Safeguard |
|---|---|---|---|
| Hetzner Online GmbH | Hosting and DNS | Germany | DPA in place |
| Resend | Transactional emails | USA | Standard Contractual Clauses |
| GitHub | OAuth authentication | USA | Standard Contractual Clauses |

We do **not** sell or rent your data. We do not share it for marketing.

We may disclose data:
- To comply with legal obligations (court orders, valid law-enforcement requests)
- To protect rights, property, or safety
- With your explicit consent

## 6. International Transfers

Data is primarily stored in Germany (Hetzner data centre, Nürnberg).
Resend and GitHub are based in the United States; transfers to them are
protected by Standard Contractual Clauses adopted by the European
Commission.

## 7. Security

Technical measures include:
- TLS 1.2+ for all connections
- Encrypted credentials at rest (AES-256-GCM for target authentication secrets)
- Hashed passwords and tokens (Argon2id)
- Session cookies marked HttpOnly, Secure, SameSite=Lax
- IP addresses hashed before storage
- Application errors logged without request bodies
- Daily automated security patches via Docker image rebuilds

We will notify affected users without undue delay if we become aware of
a personal-data breach affecting your data, and we will notify the
competent supervisory authority within 72 hours where required.

## 8. Your Rights

Under GDPR, you have the right to:

- **Access** your personal data (Article 15) — see §10
- **Rectify** inaccurate data (Article 16) — update via /settings
- **Erase** your data (Article 17) — see §10 ("right to be forgotten")
- **Restrict** processing (Article 18) — contact us
- **Data portability** (Article 20) — see §10
- **Object** to processing based on legitimate interest (Article 21) —
  contact us
- **Withdraw consent** (Article 7(3)) — applies only if we relied on
  consent for processing
- **Lodge a complaint** with your local supervisory authority

## 9. Cookies

We use one cookie: `_sm_session`, which holds your session identifier.
This is **strictly necessary** for the Service to function and does not
require consent.

We do not use analytics, advertising, or third-party tracking cookies.

See our [Cookie Policy](/cookies) for details.

## 10. Data Subject Requests

Two channels — use whichever is convenient:

**Self-service (recommended):**

- **Export:** Visit /settings/account → "Export My Data". You receive a
  JSON file with all data associated with your account.
- **Deletion:** Visit /settings/account → "Delete My Account". The account
  is immediately suspended and permanently purged after 30 days.

**Email:** Send a request to slima4.u8@gmail.com. We will:

- Acknowledge receipt within 7 days
- Verify your identity (typically: email match with account email)
- Fulfil your request within 30 days

You can use the email channel if you are locked out of your account, if
you are acting on behalf of someone else (e.g., deceased user), or if
you have requirements beyond what the self-service tools provide.

## 11. Children

The Service is not directed to children under 16. We do not knowingly
collect data from children under 16. If you become aware that a child
has provided us with personal data without parental consent, please
contact us so we can delete it.

## 12. Changes

We may update this Policy. Material changes will be announced via email
30 days in advance.

## 13. Contact

slima4.u8@gmail.com
