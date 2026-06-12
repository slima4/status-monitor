//! Server-rendered authentication pages: `/login` and `/onboarding/org`.
//!
//! These pages do not perform any auth themselves — the login button hands
//! off to `/auth/github/login`, and onboarding requires the user already to be
//! logged in (otherwise we redirect them to /login).
//!
//! `/login`: shown when an operator needs to authenticate. The GitHub button
//! preserves `redirect_after` and `invitation` query params so a bookmarked
//! invitation link survives the OAuth dance.
//!
//! `/onboarding/org`: brand-new users land here after their first OAuth login.
//! The signup org row already exists (created in Phase C of the callback);
//! all this page does is invite them to rename it from the auto-generated
//! `{adj}-{noun}-{suffix}` slug to something they'll recognize.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::url::{safe_redirect_target, url_encode};
use crate::error::AppError;
use crate::storage::orgs::get_org;
use crate::storage::users as users_store;
use crate::web::auth::Session;
use crate::web::error::WebResult;
use crate::web::filters;

/// Sentinel matched against `nav` in base.html so the header doesn't render
/// "Dashboard" / "Targets" links on the bare login page.
const TAB_LOGIN: &str = "login";
const TAB_ONBOARD: &str = "onboarding";
const TAB_SETTINGS: &str = "settings";
const TAB_USAGE: &str = "usage";
const TAB_ACCOUNT: &str = "account";

#[derive(Debug, Default, Deserialize)]
pub struct LoginQuery {
    pub redirect_after: Option<String>,
    pub invitation: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "auth/login.html")]
pub struct LoginPage {
    pub active_tab: &'static str,
    pub github_enabled: bool,
    pub github_url: String,
    pub invitation_hint: Option<String>,
    /// Cached, timeout-bounded `target_store.ping()` — same dependency check
    /// as `/readyz`, non-sensitive (no tenant scope). See [`login_ready`].
    pub ready: bool,
}

pub async fn login(State(state): State<AppState>, Query(q): Query<LoginQuery>) -> LoginPage {
    let cfg = &state.cfg.auth.github;
    let github_enabled = !cfg.client_id.is_empty() && !cfg.client_secret.expose_secret().is_empty();

    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(r) = q.redirect_after.as_deref().and_then(safe_redirect_target) {
        params.push(("redirect_after", r.to_string()));
    }
    if let Some(inv) = q.invitation.as_deref() {
        params.push(("invitation", inv.to_string()));
    }
    let github_url = if params.is_empty() {
        "/auth/github/login".to_string()
    } else {
        let qs: String = params
            .iter()
            .map(|(k, v)| format!("{k}={}", url_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("/auth/github/login?{qs}")
    };

    LoginPage {
        active_tab: TAB_LOGIN,
        github_enabled,
        github_url,
        invitation_hint: q.invitation,
        ready: login_ready(&state).await,
    }
}

/// Process-global cached readiness for the (public, unauthenticated) login
/// pill. `/login` is internet-facing and unthrottled at the edge for an
/// anonymous visitor, so probing PG on *every* hit turns a cheap GET into a
/// pool-acquire amplification lever against a small connection pool. One PG
/// pool ⇒ readiness is process-wide, so a short TTL collapses a flood into
/// ≤1 probe per [`READINESS_TTL_MS`], and the probe is timeout-bounded so a
/// saturated/slow backend degrades the pill instead of hanging the page an
/// operator needs most exactly when the backend is unwell.
async fn login_ready(state: &AppState) -> bool {
    static CACHE: std::sync::OnceLock<ReadinessCache> = std::sync::OnceLock::new();
    static MONO_START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    const READINESS_TTL_MS: u64 = 5_000;
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

    struct ReadinessCache {
        ready: std::sync::atomic::AtomicBool,
        // 0 = never probed (forces a first probe); else ms since MONO_START.
        checked_at_ms: std::sync::atomic::AtomicU64,
    }
    use std::sync::atomic::Ordering::Relaxed;

    let cache = CACHE.get_or_init(|| ReadinessCache {
        ready: std::sync::atomic::AtomicBool::new(false),
        checked_at_ms: std::sync::atomic::AtomicU64::new(0),
    });
    let now = (MONO_START.elapsed().as_millis() as u64).max(1);
    let last = cache.checked_at_ms.load(Relaxed);
    if last != 0 && now.saturating_sub(last) < READINESS_TTL_MS {
        return cache.ready.load(Relaxed);
    }

    // A few requests may race here at expiry and each probe once — bounded
    // by TTL, not per-request, which is the property that matters.
    let ready = tokio::time::timeout(PROBE_TIMEOUT, state.target_store.ping())
        .await
        .is_ok_and(|r| r.is_ok());
    cache.ready.store(ready, Relaxed);
    cache.checked_at_ms.store(now, Relaxed);
    ready
}

#[derive(Template, WebTemplate)]
#[template(path = "auth/onboarding.html")]
pub struct OnboardingPage {
    pub active_tab: &'static str,
    pub display_name: String,
    pub org_id: String,
    pub current_name: String,
}

pub async fn onboarding_org(
    State(state): State<AppState>,
    session: Session,
) -> WebResult<Response> {
    let Some(user) = session.user.clone() else {
        return Ok(crate::web::auth::login_redirect("/onboarding/org").into_response());
    };
    let pool = state.require_db()?;
    if users_store::is_onboarding_complete(pool, user.id).await? {
        return Ok(Redirect::to("/").into_response());
    }
    let Some(org_id) = users_store::resolve_signup_org(pool, user.id).await? else {
        return Ok(Redirect::to("/").into_response());
    };
    let org = get_org(pool, org_id).await?.ok_or_else(|| {
        AppError::Other(anyhow::anyhow!("default org row missing for {org_id:?}"))
    })?;

    let display_name = display_name_for(&user.email);
    Ok(OnboardingPage {
        active_tab: TAB_ONBOARD,
        display_name,
        org_id: org.id.0.to_string(),
        current_name: org.name,
    }
    .into_response())
}

/// First char upper-cased, the rest unchanged. Empty input → empty string.
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Crude "Alice" from "alice@example.com" — fine for the welcome screen;
/// we'll let the user pick their real display name in /settings later.
fn display_name_for(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("there");
    let first = local
        .split(&['.', '_', '-', '+'][..])
        .next()
        .unwrap_or(local);
    let capitalized = capitalize_first(first);
    if capitalized.is_empty() {
        "There".to_string()
    } else {
        capitalized
    }
}

pub mod settings {
    use askama::Template;
    use askama_web::WebTemplate;
    use axum::extract::State;
    use axum::response::{IntoResponse, Redirect, Response};

    use crate::app::AppState;
    use crate::auth::{account, api_tokens, session as session_store};
    use crate::error::AppError;
    use crate::storage::orgs::list_orgs_for_user;
    use crate::web::auth::{CurrentOrg, Session};
    use crate::web::error::WebResult;
    use crate::web::filters;
    use crate::web::views::resolve_org;

    use super::{TAB_ACCOUNT, TAB_SETTINGS, TAB_USAGE};

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/sessions.html")]
    pub struct SessionsPage {
        pub active_tab: &'static str,
    }

    pub struct OrgOption {
        pub slug: String,
        pub name: String,
        pub selected: bool,
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/api_tokens.html")]
    pub struct ApiTokensPage {
        pub active_tab: &'static str,
        pub orgs: Vec<OrgOption>,
    }

    pub struct SessionRow {
        /// SHA-256 hex of the cookie. Surfaced into the revoke form URL —
        /// safe because the cookie's 256-bit pre-image can't be derived.
        pub id_hash: String,
        pub created: chrono::DateTime<chrono::Utc>,
        pub last_used: chrono::DateTime<chrono::Utc>,
        pub expires: chrono::DateTime<chrono::Utc>,
        pub ip_short: String,
        pub is_current: bool,
    }

    pub struct TokenRow {
        pub id: String,
        pub name: String,
        pub prefix: String,
        pub created: chrono::DateTime<chrono::Utc>,
        pub last_used: Option<chrono::DateTime<chrono::Utc>>,
        pub expires: Option<chrono::DateTime<chrono::Utc>>,
        pub expired: bool,
        pub access: &'static str,
        /// Sorted scope list for the access-cell tooltip.
        pub scopes: String,
        pub org: Option<String>,
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/sessions_partial.html")]
    pub struct SessionsPartial {
        pub sessions: Vec<SessionRow>,
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/api_tokens_partial.html")]
    pub struct TokensPartial {
        pub tokens: Vec<TokenRow>,
    }

    pub async fn sessions_page(session: Session) -> Response {
        if session.user.is_none() {
            return crate::web::auth::login_redirect("/settings/sessions").into_response();
        }
        SessionsPage {
            active_tab: TAB_SETTINGS,
        }
        .into_response()
    }

    pub async fn api_tokens_page(
        State(state): State<AppState>,
        session: Session,
    ) -> WebResult<Response> {
        let Some(user) = session.user.as_ref() else {
            return Ok(crate::web::auth::login_redirect("/settings/api-tokens").into_response());
        };
        let pool = state.require_db()?;
        let mut orgs: Vec<OrgOption> = list_orgs_for_user(pool, user.id)
            .await?
            .into_iter()
            .map(|o| OrgOption {
                selected: Some(o.org.id) == session.active_org_id,
                slug: o.org.slug,
                name: o.org.name,
            })
            .collect();
        // Bind-by-default: if the active org didn't match (or none is set), pin
        // the dropdown to the first org rather than letting the browser pick it.
        if !orgs.iter().any(|o| o.selected)
            && let Some(first) = orgs.first_mut()
        {
            first.selected = true;
        }
        Ok(ApiTokensPage {
            active_tab: TAB_SETTINGS,
            orgs,
        }
        .into_response())
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/account.html")]
    pub struct AccountPage {
        pub active_tab: &'static str,
        pub email: String,
        /// Identity provider label, e.g. `GitHub`, or `—` if none on file.
        pub provider: String,
        pub joined: Option<chrono::DateTime<chrono::Utc>>,
        pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
        pub theme: String,
        pub time_format: String,
    }

    /// `GET /settings/account`. An unauthenticated hit redirects to login
    /// (matching the other settings pages); a DB error surfaces as the 5xx
    /// page. Export and delete are driven from the page against the API —
    /// this handler only renders the chrome plus the read-only overview.
    pub async fn account_page(
        State(state): State<AppState>,
        session: Session,
    ) -> WebResult<Response> {
        let Some(user) = session.user.clone() else {
            return Ok(crate::web::auth::login_redirect("/settings/account").into_response());
        };
        let pool = state.require_db()?;
        let (joined, provider, last_seen) = match account::account_facts(pool, user.id).await? {
            Some(f) => (
                Some(f.created_at),
                provider_label(f.provider.as_deref()),
                f.last_seen_at,
            ),
            None => (None, "—".to_string(), None),
        };
        let prefs = crate::storage::users::get_display_prefs(pool, user.id).await?;
        Ok(AccountPage {
            active_tab: TAB_ACCOUNT,
            email: user.email,
            provider,
            joined,
            last_seen,
            theme: prefs.theme.as_str().to_string(),
            time_format: prefs.time_format.as_str().to_string(),
        }
        .into_response())
    }

    fn provider_label(p: Option<&str>) -> String {
        match p {
            Some("github") => "GitHub".to_string(),
            Some(other) if !other.is_empty() => super::capitalize_first(other),
            _ => "—".to_string(),
        }
    }

    pub async fn sessions_partial(
        State(state): State<AppState>,
        session: Session,
    ) -> WebResult<Response> {
        let Some(user) = session.user.as_ref() else {
            return Ok(Redirect::to("/login").into_response());
        };
        let pool = state.require_db()?;
        let rows = session_store::list_for_user(pool, user.id).await?;
        let current = session.session_id_hash.as_deref();
        let sessions = rows
            .into_iter()
            .map(|r| SessionRow {
                ip_short: short_hash(r.ip_hash.as_deref()),
                created: r.created_at,
                last_used: r.last_used_at,
                expires: r.expires_at,
                is_current: Some(r.id_hash.as_str()) == current,
                id_hash: r.id_hash,
            })
            .collect();
        Ok(SessionsPartial { sessions }.into_response())
    }

    pub async fn api_tokens_partial(
        State(state): State<AppState>,
        session: Session,
    ) -> WebResult<Response> {
        let Some(user) = session.user.as_ref() else {
            return Ok(Redirect::to("/login").into_response());
        };
        let pool = state.require_db()?;
        let now = chrono::Utc::now();
        let rows = api_tokens::list_for_user(pool, user.id).await?;
        let tokens = rows
            .into_iter()
            .map(|r| TokenRow {
                id: r.id.to_string(),
                name: r.name,
                prefix: r.token_prefix,
                created: r.created_at,
                last_used: r.last_used_at,
                expired: r.expires_at.is_some_and(|e| e <= now),
                expires: r.expires_at,
                access: access_label(&r.scopes.0),
                scopes: {
                    let mut s = r.scopes.0.clone();
                    s.sort();
                    s.join(", ")
                },
                org: r.org_slug,
            })
            .collect();
        Ok(TokensPartial { tokens }.into_response())
    }

    /// Only an exact all-resources `:read` / `:write` set earns a preset word;
    /// any narrower Advanced grant is `custom` (exact scopes in the tooltip).
    fn access_label(scopes: &[String]) -> &'static str {
        use crate::auth::scope::Scope;
        use std::collections::HashSet;

        if scopes.iter().any(|s| s == Scope::FullAccess.as_str()) {
            return "full access";
        }
        const READ_PRESET: [&str; 5] = [
            Scope::TargetsRead.as_str(),
            Scope::ChannelsRead.as_str(),
            Scope::IncidentsRead.as_str(),
            Scope::MaintenanceRead.as_str(),
            Scope::StatusPageRead.as_str(),
        ];
        const WRITE_PRESET: [&str; 5] = [
            Scope::TargetsWrite.as_str(),
            Scope::ChannelsWrite.as_str(),
            Scope::IncidentsWrite.as_str(),
            Scope::MaintenanceWrite.as_str(),
            Scope::StatusPageWrite.as_str(),
        ];
        let have: HashSet<&str> = scopes.iter().map(String::as_str).collect();
        let is_preset =
            |preset: &[&str]| have.len() == preset.len() && preset.iter().all(|s| have.contains(s));

        if is_preset(&READ_PRESET) {
            "read-only"
        } else if is_preset(&WRITE_PRESET) {
            "read & write"
        } else {
            "custom"
        }
    }

    /// Compact binary-unit label for help text (e.g. `1 MB`, `512 KB`).
    /// Floors to the largest whole unit; operator-set limits are expected
    /// round, so the floor is exact in practice.
    fn human_bytes(b: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = 1024 * KB;
        if b >= MB {
            format!("{} MB", b / MB)
        } else if b >= KB {
            format!("{} KB", b / KB)
        } else {
            format!("{b} B")
        }
    }

    /// Hard cap on rendered quota cells. One cell per unit up to this; a
    /// larger plan falls back to a proportional `CELL_CAP`-wide meter so the
    /// row stays legible instead of rendering hundreds of hairline cells.
    const CELL_CAP: i64 = 64;

    /// `cells[i] == true` for the first `round(pct% × total)` cells — the
    /// proportion-filled flags the segmented meter iterates.
    fn filled_flags(pct: i64, total: i64) -> Vec<bool> {
        let on = ((pct * total + 50) / 100).clamp(0, total);
        (0..total).map(|i| i < on).collect()
    }

    /// One cell per unit of `limit`, the first `current` filled — so the
    /// user can literally count cells against the cap. Falls back to a
    /// proportional `CELL_CAP`-wide meter once the limit exceeds the cap.
    fn quota_cells(current: i64, limit: i64, pct: i64) -> Vec<bool> {
        if limit <= 0 {
            Vec::new()
        } else if limit <= CELL_CAP {
            let on = current.clamp(0, limit);
            (0..limit).map(|i| i < on).collect()
        } else {
            filled_flags(pct, CELL_CAP)
        }
    }

    /// One quota row. `pct` is pre-clamped 0–100 in Rust so the template
    /// stays logic-free; `limit_display` shows ∞ for the synthetic unlimited
    /// (self-host) plan instead of a meaningless 2.1-billion. `fill_class`
    /// maps pct → ok/warn/bad CSS variant (`<80`, `80–94`, `≥95`).
    /// `unlimited` swaps the segmented meter for an open-ended ∞ rail;
    /// `cells` is the per-segment filled state (empty when unlimited).
    pub struct UsageBar {
        pub label: &'static str,
        pub current: i64,
        pub limit_display: String,
        pub pct: i64,
        pub fill_class: &'static str,
        pub unlimited: bool,
        pub cells: Vec<bool>,
    }

    impl UsageBar {
        fn new(label: &'static str, current: i64, limit: i32) -> Self {
            let unlimited = limit == i32::MAX;
            let limit = i64::from(limit);
            let pct = if unlimited || limit <= 0 {
                0
            } else {
                (current * 100 / limit).clamp(0, 100)
            };
            let fill_class = if unlimited {
                "ok"
            } else {
                match pct {
                    100 => "bad",
                    p if p >= 80 => "warn",
                    _ => "ok",
                }
            };
            Self {
                label,
                current,
                limit_display: if unlimited {
                    "∞".to_string()
                } else {
                    limit.to_string()
                },
                pct,
                fill_class,
                unlimited,
                cells: if unlimited {
                    Vec::new()
                } else {
                    quota_cells(current, limit, pct)
                },
            }
        }
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/usage.html")]
    pub struct UsagePage {
        pub active_tab: &'static str,
        pub plan_name: String,
        pub bars: Vec<UsageBar>,
        pub min_check_interval_secs: i32,
        pub retention_days: i32,
        pub max_logo_size_label: String,
        pub max_api_tokens_per_user: i32,
        pub api_writes_per_minute: i32,
        pub api_reads_per_minute: i32,
        pub bulk_ops_per_minute: i32,
        pub test_now_per_minute: i32,
        pub check_now_per_minute: i32,
    }

    /// `GET /settings/usage`. Auth/redirect behaviour mirrors
    /// `/settings/status-page`: only an *unauthenticated* hit bounces to
    /// login; the org resolves exactly as the API does, so the page and
    /// `GET /api/v1/orgs/{id}/usage` always show the same numbers.
    pub async fn usage_page(
        State(state): State<AppState>,
        org: Result<CurrentOrg, AppError>,
    ) -> WebResult<Response> {
        let org = match resolve_org(org, "/settings/usage") {
            Ok(o) => o,
            Err(resp) => return Ok(*resp),
        };
        let u = state.quotas.org_usage(org).await?;
        let p = &u.plan;
        Ok(UsagePage {
            active_tab: TAB_USAGE,
            plan_name: p.name.clone(),
            bars: vec![
                UsageBar::new("targets", u.targets, p.max_targets),
                UsageBar::new("members", u.members, p.max_members),
                UsageBar::new(
                    "public-components",
                    u.public_components,
                    p.max_public_components,
                ),
                UsageBar::new(
                    "pending-invitations",
                    u.pending_invitations,
                    p.max_pending_invitations,
                ),
                UsageBar::new(
                    "maintenance-windows",
                    u.maintenance_windows,
                    p.max_maintenance_windows,
                ),
                UsageBar::new(
                    "notification-channels",
                    u.notification_channels,
                    p.max_notification_channels,
                ),
            ],
            min_check_interval_secs: p.min_check_interval_secs,
            retention_days: p.retention_days,
            max_logo_size_label: human_bytes(u64::try_from(p.max_logo_size_bytes).unwrap_or(0)),
            max_api_tokens_per_user: p.max_api_tokens_per_user,
            api_writes_per_minute: p.api_writes_per_minute,
            api_reads_per_minute: p.api_reads_per_minute,
            bulk_ops_per_minute: p.bulk_ops_per_minute,
            test_now_per_minute: p.test_now_per_minute,
            check_now_per_minute: p.check_now_per_minute,
        }
        .into_response())
    }

    fn short_hash(h: Option<&str>) -> String {
        match h {
            Some(s) if s.len() >= 12 => s[..12].to_string(),
            Some(s) => s.to_string(),
            None => "—".to_string(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn usage_page_renders_progress_bars_and_contact_link() {
            let html = UsagePage {
                active_tab: super::super::TAB_USAGE,
                plan_name: "Free".into(),
                bars: vec![
                    UsageBar::new("Targets", 7, 10),
                    UsageBar::new("Members", 1, i32::MAX),
                ],
                min_check_interval_secs: 60,
                retention_days: 90,
                max_logo_size_label: "1 MB".into(),
                max_api_tokens_per_user: 5,
                api_writes_per_minute: 600,
                api_reads_per_minute: 6000,
                bulk_ops_per_minute: 30,
                test_now_per_minute: 60,
                check_now_per_minute: 60,
            }
            .render()
            .unwrap();
            assert!(html.starts_with("<!doctype html>"));
            // Plan rendered as an ink token in the header.
            assert!(html.contains(r#"class="usage-plan">plan:free"#));
            // Bounded quota: the figures, the progressbar value, and at
            // least one filled cell in the segmented meter.
            assert!(html.contains(r#"usage-grid__fig">7</span>"#));
            assert!(html.contains(r#"usage-grid__fig">10</span>"#));
            assert!(html.contains(r#"aria-valuenow="70""#));
            assert!(html.contains("usage-cells__c--on-"));
            // Unlimited (self-host) cap renders ∞ + a single empty cell.
            assert!(html.contains(r#"usage-grid__fig">∞</span>"#));
            assert!(html.contains(r#"aria-label="unlimited""#));
            assert!(html.contains("60s"));
            assert!(html.contains(r#"href="mailto:support@uptimepage.dev""#));
        }

        #[test]
        fn account_page_renders_privacy_and_sessions_sections() {
            let html = AccountPage {
                active_tab: super::super::TAB_ACCOUNT,
                email: "alice@example.com".into(),
                provider: "GitHub".into(),
                joined: Some("2026-02-14T09:00:00Z".parse().unwrap()),
                last_seen: Some("2026-05-16T12:00:00Z".parse().unwrap()),
                theme: "default".into(),
                time_format: "auto".into(),
            }
            .render()
            .unwrap();
            assert!(html.starts_with("<!doctype html>"));
            assert!(html.contains("alice@example.com"));
            assert!(html.contains("via GitHub"));
            assert!(html.contains("2026-02-14 09:00 UTC"));
            // Export is a real download link to the API, not an HTMX swap.
            assert!(html.contains(r#"href="/api/v1/me/data-export""#));
            assert!(html.contains("download"));
            // Delete drives the modal, which DELETEs /api/v1/me.
            assert!(html.contains(r#"id="delete-modal""#));
            assert!(html.contains(r#"hx-delete="/api/v1/me""#));
            // Sessions section reuses the shared partial loader (no dup logic).
            assert!(html.contains(r#"hx-get="/web/partials/settings/sessions""#));
            assert!(html.contains("logout-all"));
        }

        #[test]
        fn provider_label_maps_known_and_unknown() {
            assert_eq!(provider_label(Some("github")), "GitHub");
            assert_eq!(provider_label(Some("gitlab")), "Gitlab");
            assert_eq!(provider_label(Some("")), "—");
            assert_eq!(provider_label(None), "—");
        }

        #[test]
        fn sessions_page_renders_chrome_and_partial_hook() {
            let html = SessionsPage {
                active_tab: super::super::TAB_SETTINGS,
            }
            .render()
            .unwrap();
            assert!(html.starts_with("<!doctype html>"));
            assert!(html.contains("active sessions"));
            assert!(html.contains(r#"hx-get="/web/partials/settings/sessions""#));
            assert!(html.contains("logout-all"));
        }

        #[test]
        fn api_tokens_page_renders_create_form_and_partial_hook() {
            let html = ApiTokensPage {
                active_tab: super::super::TAB_SETTINGS,
                orgs: vec![OrgOption {
                    slug: "acme".into(),
                    name: "Acme".into(),
                    selected: true,
                }],
            }
            .render()
            .unwrap();
            assert!(html.contains("API tokens"));
            assert!(html.contains(r#"id="new-token-form""#));
            assert!(html.contains("/api/v1/me/api-tokens"));
            assert!(html.contains(r#"<option value="acme" selected>Acme</option>"#));
            assert!(html.contains(r#"hx-get="/web/partials/settings/api-tokens""#));
        }

        #[test]
        fn sessions_partial_renders_empty_state() {
            let html = SessionsPartial { sessions: vec![] }.render().unwrap();
            assert!(html.contains("# no active sessions"));
            // Partial must not include the page chrome — it's swapped in via HTMX.
            assert!(!html.contains("<!doctype html>"));
        }

        #[test]
        fn sessions_partial_marks_current_session() {
            let html = SessionsPartial {
                sessions: vec![SessionRow {
                    id_hash: "abc".into(),
                    created: "2026-05-16T12:00:00Z".parse().unwrap(),
                    last_used: "2026-05-16T12:00:00Z".parse().unwrap(),
                    expires: "2026-06-16T12:00:00Z".parse().unwrap(),
                    ip_short: "deadbeefcafe".into(),
                    is_current: true,
                }],
            }
            .render()
            .unwrap();
            assert!(html.contains("this device"));
            assert!(!html.contains("hx-delete"));
        }

        #[test]
        fn tokens_partial_renders_revoke_when_present() {
            let html = TokensPartial {
                tokens: vec![TokenRow {
                    id: "tok-1".into(),
                    name: "CI".into(),
                    prefix: "sm_live_aaaaaaaa".into(),
                    created: "2026-05-16T12:00:00Z".parse().unwrap(),
                    last_used: None,
                    expires: None,
                    expired: false,
                    access: "read-only",
                    scopes: "targets:read".into(),
                    org: Some("acme".into()),
                }],
            }
            .render()
            .unwrap();
            assert!(html.contains("CI"));
            assert!(html.contains("sm_live_aaaaaaaa"));
            assert!(html.contains("read-only"));
            assert!(html.contains("acme"));
            assert!(html.contains(r#"hx-delete="/api/v1/me/api-tokens/tok-1""#));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_page_renders_github_button_when_enabled() {
        let html = LoginPage {
            active_tab: TAB_LOGIN,
            github_enabled: true,
            github_url: "/auth/github/login".into(),
            invitation_hint: None,
            ready: true,
        }
        .render()
        .unwrap();
        assert!(html.contains("continue with github"));
        assert!(html.contains(r#"href="/auth/github/login""#));
        assert!(!html.contains("not configured"));
        // Login page suppresses the user-area nav so a not-yet-authenticated
        // visitor doesn't see broken "Settings"/"Log out" controls.
        assert!(!html.contains("Log out"));
    }

    #[test]
    fn login_page_shows_warning_when_oauth_not_configured() {
        let html = LoginPage {
            active_tab: TAB_LOGIN,
            github_enabled: false,
            github_url: "/auth/github/login".into(),
            invitation_hint: None,
            ready: true,
        }
        .render()
        .unwrap();
        assert!(html.contains("not configured"));
        assert!(!html.contains("continue with github"));
    }

    #[test]
    fn login_page_shows_invitation_hint() {
        let html = LoginPage {
            active_tab: TAB_LOGIN,
            github_enabled: true,
            github_url: "/auth/github/login?invitation=abc".into(),
            invitation_hint: Some("abc".into()),
            ready: true,
        }
        .render()
        .unwrap();
        assert!(html.contains("After signing in"));
        assert!(html.contains("abc"));
    }

    #[test]
    fn onboarding_page_renders_form_with_org_name() {
        let html = OnboardingPage {
            active_tab: TAB_ONBOARD,
            display_name: "Alice".into(),
            org_id: "00000000-0000-0000-0000-000000000001".into(),
            current_name: "quiet-koala-7m0tt1".into(),
        }
        .render()
        .unwrap();
        assert!(html.contains("welcome, Alice"));
        assert!(html.contains(r#"value="quiet-koala-7m0tt1""#));
        assert!(html.contains(r#"hx-patch="/api/v1/orgs/00000000-0000-0000-0000-000000000001""#));
        assert!(html.contains(r#""X-Requested-With":"uptimepage""#));
    }

    #[test]
    fn display_name_from_email() {
        assert_eq!(display_name_for("alice@example.com"), "Alice");
        assert_eq!(display_name_for("alice.smith@example.com"), "Alice");
        assert_eq!(display_name_for("bob_jones@example.com"), "Bob");
        assert_eq!(display_name_for("@example.com"), "There");
    }
}
