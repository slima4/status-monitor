# Authentication

uptimepage ships with an in-binary auth stack: GitHub and Google OAuth
for the operator UI, opaque per-user API tokens for the REST surface, and
magic-link sign-in (enabled by default) for users without an OAuth identity. The
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
- **Magic-link token.** A single-use row in `magic_link_tokens`
  (`auth.magic_link.expiry_minutes`, default 15). Enabled by default;
  gated by `auth.enabled_methods`.

## Flows

### OAuth sign-in (GitHub, Google)

Both providers share one callback runner; only the upstream identity
fetch differs. The callback is split into three strict phases:

1. **Phase A** — `DELETE … RETURNING` consumes the `oauth_states` row in
   one statement (provider-bound: a state minted for one provider cannot
   complete another's callback). No upstream call has happened yet, so
   the DB connection is released before any HTTP.
2. **Phase B** — exchange `code` for an access token, then fetch the
   profile: GitHub `/user` + `/user/emails` (verified primary only),
   Google OIDC userinfo (email accepted only with `email_verified`).
   No DB connection is held.
3. **Phase C** — a fresh transaction materialises the user + identity,
   links a new provider to an existing account on verified-email match
   (a soft-deleted account matches, and stays deleted), auto-creates a
   signup org if this is a new sign-up, and commits. The user's default
   org (oldest active membership) is resolved after commit for the
   session row.

After commit, the previous session cookie (if any) is destroyed for
session-fixation defence, a fresh session row is INSERTed, the cookie is
set, and the user is redirected. A sign-in on an account scheduled for
deletion redirects to `/account/restore` and nothing else: signing in
proves who is asking, and cancelling the deletion is a separate,
deliberate `POST /api/v1/me/restore`. Failure modes:

- Invalid or expired state → 400 `INVALID_STATE`, logged to
  `login_attempts`.
- User denied consent / provider sent no code → redirect back to
  `/login`, logged with `failure_reason = "oauth_denied"` (or
  `"missing_code"`).
- Upstream failure → 500, logged with
  `failure_reason = "oauth_upstream_failed"` (rows from before 2026-06
  carry the old `"github_upstream_failed"`).
- Disabled (`enabled_methods`) or incompletely configured provider →
  404 `AUTH_METHOD_UNAVAILABLE` on both start and callback; the
  listed-but-misconfigured case logs a warning.

### API token auth

Bearer tokens skip the cookie path entirely. The middleware checks the
`Authorization: Bearer …` header against the `api_tokens` table via the
indexed `token_prefix` (first 16 chars of the raw token), then
argon2-verifies the survivor. `last_used_at` is updated through the same
60-second debounce as session cookies.

CSRF protection does not apply: cross-origin browsers don't auto-attach
the `Authorization` header, so there is no forgery surface.

To manage resources with a token as code, see [Terraform](terraform.md). To let an LLM client query and act on an org with a token, see the [MCP server](mcp.md).

#### Scopes

Every token carries a set of `resource:action` scopes. A request is rejected with `403 INSUFFICIENT_SCOPE` unless the token holds the scope its endpoint requires. `full_access` is a superset that grants all of them; unknown scope strings are ignored (forward-compatible).

| Resource | `read` | `write` | `delete` | `execute` |
|---|---|---|---|---|
| `targets` | list / get / results / uptime / latency / incident history | create / update / bulk | delete, bulk-delete | run a check now, test-probe a config |
| `channels` | list / get | create / update | delete | send a test notification |
| `incidents` | incident list / detail / delivery log / metrics / postmortem (the public timeline needs no token) | narrate / post update, acknowledge / resolve | — | — |
| `maintenance` | list / get | create / update | delete | — |
| `status_page` | read settings | update settings, upload logo | remove logo | — |
| `variables` | list / get (secret values redacted) | create / rotate | delete (blocked while referenced) | — |
| `oncall` | view escalation policies and on-call schedules | manage them (owner-only) | — | — |

`write` implies `read` for the same resource. `delete` and `execute` are **independent** — they are *not* granted by `write`, so a config-management token (`*:write`) can change resources but cannot destroy them or trigger side effects. Grant `delete`/`execute` explicitly when you need them.

#### Org binding

A token is user-scoped, so each request names an org via the `X-Uptimepage-Org: <slug>` header. A token can additionally be **bound** to one org at creation:

- **Bound** — the header is optional; if sent it must name the bound org, else `403 ORG_HEADER_MISMATCH`. The token can never act on the user's other orgs.
- **Unbound** — the header is required (`400 ORG_REQUIRED` if absent). A malformed/unknown slug is `400 ORG_HEADER_INVALID` on either kind.

#### Expiry

A token may carry an expiry (1–365 days); an expired token authenticates as invalid. Tokens without an expiry never lapse — prefer a bounded lifetime.

#### Managing tokens

Token management — create, list, rename, revoke — is **browser-session only**: these endpoints read the session cookie and reject bearer tokens, so a token can never mint another token (which would escape its own scopes) or reach account/org administration. Mint tokens in the UI at **Settings → API tokens** (a verified email is required).

### Magic-link sign-in (gated)

Available only when `auth.enabled_methods` contains `"magic_link"`:

1. `POST /auth/magic-link/request {email}` — generates a 32-byte token,
   hashes it, INSERTs into `magic_link_tokens` with a 15-minute expiry,
   and emails the verify URL via the configured `EmailSender`.
   Anti-enumeration: the response is identical for known, unknown, and
   malformed emails — `{"sent": true}`.
2. `GET /auth/magic-link/verify?token=…` — atomically marks the row
   `used_at = now()`, destroys any pre-login session, mints a new
   session, auto-accepts a carried invitation, and redirects by
   priority: `/account/restore` (the account is scheduled for deletion;
   signing in does not cancel that, so the choice gets its own page) →
   `/?joined=<slug>` → `/?invite=missed` (carried invitation failed to
   redeem) → carried `redirect_after` → `/`. An invalid, used, or
   expired token renders an HTML "link expired" page with status 410 —
   one indistinguishable state, no JSON error envelope.

The schema and email template ship in v1 even when the flow is gated, so
flipping the config doesn't require a migration.

### Invitations

Owners issue invitations to email addresses. The recipient gets emailed
accept/decline links embedding the raw token (single-use, hashed at rest
with the same argon2id parameters as API tokens).

- `GET /invitations/accept?token=…` — with a session, redeems right
  there (clicking the emailed link is the consent; email must match);
  without one, bounces to `/login?invitation=…` and every sign-in
  method carries the invitation through and auto-accepts after login.
  The session's active org rotates to the joined org and the dashboard
  shows a "welcome to <org>" banner (`/?joined=<slug>`). A carried
  invitation that can't be redeemed (mismatched email, seat race,
  revoked) never breaks the login — the dashboard shows a generic
  "invitation couldn't be applied" banner instead.
- `GET /invitations/decline?token=…` — render-only confirm page (mail
  scanners prefetch links, so the GET never mutates); its button POSTs
  the decline.
- A magic link requested for an **unknown** email that carries a valid
  invitation for that same address bootstraps the account at verify
  time: user created (verified, consent stamped, no personal org) and
  joined directly into the inviter's org. Without a matching
  invitation, unknown emails still get the indistinguishable
  invalid-link page.
- A seat-race loser's invitation is un-consumed (`accepted_at`
  reverted), so "try again once a seat frees up" stays true.

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
| `GET`  | `/auth/google/login`           | none    | Initiate Google OAuth |
| `GET`  | `/auth/google/callback`        | none    | Handle Google OAuth callback |
| `GET`  | `/invitations/accept`          | optional session | Emailed accept link (HTML; redeems with session, else login bounce) |
| `GET`  | `/invitations/decline`         | none    | Emailed decline link (HTML confirm page; POST does the decline) |
| `GET`  | `/api/v1/me`                   | session/token | Current user info |
| `DELETE` | `/api/v1/me`                 | session | Delete account (soft, 30-day grace) |
| `POST` | `/api/v1/me/restore`           | session for a soft-deleted account | Cancel a pending account deletion |
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
