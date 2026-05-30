# Authentication

uptimepage ships with an in-binary auth stack: GitHub OAuth for the
operator UI, opaque per-user API tokens for the REST surface, and
optional magic-link sign-in for users without a GitHub identity. The
binary always runs as multi-tenant SaaS — single-tenant deployments are
just SaaS with one signed-up user; see [Multi-tenancy](multi-tenancy.md)
for the full model.

## Concepts

- **User.** A row in `users`, keyed by id. Email is CITEXT. A user can
  belong to multiple orgs.
- **Session.** A 32-byte random id (43 base64url chars) stored in a
  `HttpOnly; Secure; SameSite=Lax` cookie, default `_sm_session`. Backed
  by a `sessions` row with idle + absolute timeouts.
- **API token.** An opaque bearer token (`sm_live_…`) presented in the
  `Authorization: Bearer …` header. Stored as an argon2id hash plus a
  16-char prefix for indexed lookup. Returned **once** at create time and
  never again.
- **Org.** Container for the user-visible data (targets, incidents,
  maintenance, …). Memberships carry a role: `Owner`, `Member`.
- **Invitation.** A pending row in `invitations` carrying an argon2id
  hash of a single-use token sent to a prospective member's email.
- **Magic-link token.** A single-use, 15-minute, single-token row in
  `magic_link_tokens`. Disabled by default; gated by
  `auth.enabled_methods`.

## Flows

### GitHub OAuth sign-in

The callback is split into three strict phases:

1. **Phase A** — `DELETE … RETURNING` consumes the `oauth_states` row in
   one statement. No GitHub call has happened yet, so the DB connection
   is released before any HTTP.
2. **Phase B** — exchange `code` for an access token, fetch `/user` and
   `/user/emails`. No DB connection is held. Per-call timeouts come from
   `auth.github.http_connect_timeout_ms` and
   `auth.github.http_request_timeout_ms`.
3. **Phase C** — a fresh transaction materialises the user + identity,
   auto-creates a signup org if this is a new sign-up, resolves the user's
   default org (oldest active membership) for the session row, and commits.

After commit, the previous session cookie (if any) is destroyed for
session-fixation defence, a fresh session row is INSERTed, the cookie is
set, and the user is redirected. Failure modes:

- Invalid or expired state → 400 `INVALID_STATE`, logged to
  `login_attempts`.
- GitHub upstream failure → 500, logged with
  `failure_reason = "github_upstream_failed"`.

### API token auth

Bearer tokens skip the cookie path entirely. The middleware checks the
`Authorization: Bearer …` header against the `api_tokens` table via the
indexed `token_prefix` (first 16 chars of the raw token), then
argon2-verifies the survivor. `last_used_at` is updated through the same
60-second debounce as session cookies.

CSRF protection does not apply: cross-origin browsers don't auto-attach
the `Authorization` header, so there is no forgery surface.

### Magic-link sign-in (gated)

Available only when `auth.enabled_methods` contains `"magic_link"`:

1. `POST /auth/magic-link/request {email}` — generates a 32-byte token,
   hashes it, INSERTs into `magic_link_tokens` with a 15-minute expiry,
   and emails the verify URL via the configured `EmailSender`.
   Anti-enumeration: the response is identical for known, unknown, and
   malformed emails — `{"sent": true}`.
2. `GET /auth/magic-link/verify?token=…` — atomically marks the row
   `used_at = now()`, destroys any pre-login session, mints a new
   session, and redirects to `/`.

The schema and email template ship in v1 even when the flow is gated, so
flipping the config doesn't require a migration.

### Invitations

Owners issue invitations to email addresses. The recipient gets a link
embedding the raw token. Accepting requires a sign-in (GitHub or, when
enabled, magic-link). The token is single-use, hashed at rest with the
same argon2id parameters as API tokens.

## Endpoints

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET`  | `/login`                       | none    | Login page (HTML) |
| `GET`  | `/auth/github/login`           | none    | Initiate GitHub OAuth |
| `GET`  | `/auth/github/callback`        | none    | Handle OAuth callback |
| `POST` | `/auth/logout`                 | session | Destroy current session |
| `POST` | `/auth/logout-all`             | session | Destroy all sessions for current user |
| `POST` | `/auth/magic-link/request`     | none    | Request magic link (gated) |
| `GET`  | `/auth/magic-link/verify`      | none    | Verify magic-link token (gated) |
| `GET`  | `/api/v1/me`                   | session/token | Current user info |
| `GET`  | `/api/v1/me/sessions`          | session | List active sessions |
| `DELETE` | `/api/v1/me/sessions/{id}`   | session | Revoke a session |
| `GET`  | `/api/v1/me/api-tokens`        | session | List tokens (prefix only) |
| `POST` | `/api/v1/me/api-tokens`        | session | Create token (returned once) |
| `PATCH`| `/api/v1/me/api-tokens/{id}`   | session | Rename token |
| `DELETE`| `/api/v1/me/api-tokens/{id}`  | session | Revoke token |
| `POST` | `/api/v1/orgs/{org_id}/invitations` | session, owner | Issue invitation |
| `GET`  | `/api/v1/orgs/{org_id}/invitations` | session, owner | List pending |
| `DELETE`| `/api/v1/orgs/{org_id}/invitations/{id}` | session, owner | Revoke |
| `POST` | `/api/v1/invitations/accept`   | session | Accept (token in body) |
| `POST` | `/api/v1/invitations/decline`  | none    | Decline (token in body) |

## Security model

- **CSRF.** State-changing cookie-authenticated requests must carry
  `X-Requested-With: uptimepage`. Bearer requests skip. The header
  is comparison-checked in constant time via `subtle::ConstantTimeEq`.
- **Session fixation.** Both the OAuth callback and the magic-link
  verify endpoint destroy any pre-existing session bound to the browser
  before minting the new one.
- **Hashed PII.** IP addresses and User-Agent strings in
  `sessions`, `login_attempts`, and `magic_link_tokens` are stored as
  HMAC-SHA256(salt, value) — the salt lives in
  `auth.fingerprint_salt` / `auth_salt_history`. Rotating the salt
  refuses to boot without an explicit override env var to make
  audit-log breakage loud.
- **Argon2id parameters.** Default parameters from the `argon2` crate
  (`Argon2::default()`). Tokens carry 256 bits of entropy, so the
  factor of safety is in the token, not the params.
- **Anti-enumeration.** Magic-link request and invitation lookup return
  the same response whether the underlying row exists.
- **Per-email send throttle.** `auth.magic_link.rate_limit_seconds`
  (default 60) caps a single address to one outgoing email per window
  regardless of source IP. The check runs inside the spawned send
  task so it never branches the response path. Concurrent requests for
  the same address all still INSERT (preserving anti-enum work) but
  only the earliest row in the window — ordered by `(created_at, id)`
  — actually mails the user. Set to `0` to disable.

## Background workers

- `oauth_state_cleanup` — `DELETE FROM oauth_states WHERE expires_at <
  now()` every 10 minutes.
- `invitations::purge_old` — daily cleanup of accepted/declined/expired
  rows older than a configurable window.
- `magic_link_cleanup` — every 6 hours when `magic_link` is in
  `auth.enabled_methods`. Drops expired rows and used rows older than 7
  days (the forensic window for "was this token redeemed?"). When the
  method is disabled the routes 404 and no rows are ever inserted, so
  the ticker stays asleep.

## Sign-in audit

Every authentication attempt — success or failure — writes a row to
`login_attempts`:

- `method` ∈ `'github_oauth' | 'api_token' | 'magic_link'`
- `success` boolean
- `failure_reason` text (`'invalid_state'`, `'token_expired'`,
  `'invalid_token'`, …)
- `ip_hash`, `user_agent_hash` for forensic correlation without storing
  raw PII

The "recent activity" panel on the user's settings page reads from this
table.

## Deployment shape

Every authenticated request carries an active org id; data writes scope
through repositories that enforce isolation. The cross-tenant test
suite confirms a user can't read or mutate another org's rows via slug
URL or session token. Single-tenant deployments work the same way —
they just have one user and one org. See
[docs/multi-tenancy.md](multi-tenancy.md) for the data model and
isolation guarantees.
