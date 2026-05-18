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
//! The personal-org row already exists (created in Phase C of the callback);
//! all this page does is invite them to rename it from the auto-generated
//! `personal-{adj}-{noun}-{suffix}` slug-name to something they'll recognize.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::url::{safe_redirect_target, url_encode};
use crate::error::AppError;
use crate::storage::orgs::{get_org, personal_org_for_user};
use crate::web::assets::filters;
use crate::web::auth::Session;
use crate::web::error::WebResult;

/// Sentinel matched against `nav` in base.html so the header doesn't render
/// "Dashboard" / "Targets" links on the bare login page.
const TAB_LOGIN: &str = "login";
const TAB_ONBOARD: &str = "onboarding";
const TAB_SETTINGS: &str = "settings";
const TAB_STATUS_PAGE: &str = "status_page";
const TAB_USAGE: &str = "usage";
const TAB_ACCOUNT: &str = "account";
/// Suppresses the authenticated header nav (see base.html): a user reaching
/// the recovery page is signed out by construction.
const TAB_RECOVER: &str = "recover";

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
    let Some(org_id) = personal_org_for_user(pool, user.id).await? else {
        // No personal org — drop the user back to the dashboard so they don't
        // get stuck on a page that can't render.
        return Ok(Redirect::to("/").into_response());
    };
    let org = get_org(pool, org_id).await?.ok_or_else(|| {
        AppError::Other(anyhow::anyhow!("personal org row missing for {org_id:?}"))
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

#[derive(Debug, Default, Deserialize)]
pub struct RecoverQuery {
    pub token: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "auth/recover_account.html")]
pub struct RecoverAccountPage {
    pub active_tab: &'static str,
    /// Raw token from the email link; validity is decided by the API call the
    /// page makes, not here. Empty ⇒ render the invalid-link state.
    pub token: String,
}

/// `GET /recover-account?token=…`. Public (the user is signed out): renders a
/// confirm card that POSTs the token to `/api/v1/auth/recover-account`. A
/// blank token short-circuits to the invalid-link copy without an API round
/// trip.
pub async fn recover_account(Query(q): Query<RecoverQuery>) -> RecoverAccountPage {
    RecoverAccountPage {
        active_tab: TAB_RECOVER,
        token: q.token.unwrap_or_default().trim().to_string(),
    }
}

pub mod settings {
    use askama::Template;
    use askama_web::WebTemplate;
    use axum::extract::State;
    use axum::response::{IntoResponse, Redirect, Response};

    use crate::api::handlers::status_page::{
        StatusPageSettings, build_settings, load_for_settings,
    };
    use crate::app::AppState;
    use crate::auth::{account, api_tokens, session as session_store};
    use crate::error::AppError;
    use crate::web::assets::filters;
    use crate::web::auth::{CurrentOrg, Session};
    use crate::web::error::WebResult;
    use crate::web::views::{fmt_human, resolve_org};

    use super::{TAB_ACCOUNT, TAB_SETTINGS, TAB_STATUS_PAGE, TAB_USAGE};

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/sessions.html")]
    pub struct SessionsPage {
        pub active_tab: &'static str,
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/api_tokens.html")]
    pub struct ApiTokensPage {
        pub active_tab: &'static str,
    }

    pub struct SessionRow {
        pub id: String,
        pub created: String,
        pub last_used: String,
        pub expires: String,
        pub ip_short: String,
        pub is_current: bool,
    }

    pub struct TokenRow {
        pub id: String,
        pub name: String,
        pub prefix: String,
        pub created: String,
        pub last_used: String,
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

    pub async fn api_tokens_page(session: Session) -> Response {
        if session.user.is_none() {
            return crate::web::auth::login_redirect("/settings/api-tokens").into_response();
        }
        ApiTokensPage {
            active_tab: TAB_SETTINGS,
        }
        .into_response()
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/account.html")]
    pub struct AccountPage {
        pub active_tab: &'static str,
        pub email: String,
        /// Identity provider label, e.g. `GitHub`, or `—` if none on file.
        pub provider: String,
        pub joined: String,
        pub last_seen: String,
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
                fmt_human(f.created_at),
                provider_label(f.provider.as_deref()),
                f.last_login_at
                    .map(fmt_human)
                    .unwrap_or_else(|| "—".to_string()),
            ),
            None => ("—".to_string(), "—".to_string(), "—".to_string()),
        };
        Ok(AccountPage {
            active_tab: TAB_ACCOUNT,
            email: user.email,
            provider,
            joined,
            last_seen,
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
        let current = session.session_id.as_deref();
        let sessions = rows
            .into_iter()
            .map(|r| SessionRow {
                ip_short: short_hash(r.ip_hash.as_deref()),
                created: fmt_human(r.created_at),
                last_used: fmt_human(r.last_used_at),
                expires: fmt_human(r.expires_at),
                is_current: Some(r.id.as_str()) == current,
                id: r.id,
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
        let rows = api_tokens::list_for_user(pool, user.id).await?;
        let tokens = rows
            .into_iter()
            .map(|r| TokenRow {
                id: r.id.to_string(),
                name: r.name,
                prefix: r.token_prefix,
                created: fmt_human(r.created_at),
                last_used: r
                    .last_used_at
                    .map(fmt_human)
                    .unwrap_or_else(|| "never".to_string()),
            })
            .collect();
        Ok(TokensPartial { tokens }.into_response())
    }

    #[derive(Template, WebTemplate)]
    #[template(path = "settings/status_page.html")]
    pub struct StatusPageView {
        pub active_tab: &'static str,
        /// Drives the `hx-patch` / `hx-post` URLs in the form.
        pub org_id: String,
        /// Always populated (resolved override or configured default) so the
        /// `<input type=color>` has a value even before the operator picks one.
        pub brand_color_value: String,
        pub s: StatusPageSettings,
    }

    /// `GET /settings/status-page`. Only an *unauthenticated* hit redirects to
    /// login (matching the other settings pages); a Forbidden / DB error must
    /// surface as itself, not a misleading login bounce or redirect loop.
    /// `CurrentOrg` resolves the org exactly as the API does (active org, or
    /// the default org in self-host), so the form acts on the same tenant.
    pub async fn status_page(
        State(state): State<AppState>,
        org: Result<CurrentOrg, AppError>,
    ) -> WebResult<Response> {
        let org = match resolve_org(org, "/settings/status-page") {
            Ok(o) => o,
            Err(resp) => return Ok(*resp),
        };
        let pool = state.require_db()?;
        let ob = load_for_settings(pool, org).await?;
        let s = build_settings(&state, &ob);
        let brand_color_value = s
            .public_brand_color
            .clone()
            .unwrap_or_else(|| state.cfg.public_status.default_brand_color.clone());
        Ok(StatusPageView {
            active_tab: TAB_STATUS_PAGE,
            org_id: org.0.to_string(),
            brand_color_value,
            s,
        }
        .into_response())
    }

    /// One progress-bar row. `pct` is pre-clamped 0–100 in Rust so the
    /// template stays logic-free; `limit_display` shows ∞ for the synthetic
    /// unlimited (self-host) plan instead of a meaningless 2.1-billion.
    pub struct UsageBar {
        pub label: &'static str,
        pub current: i64,
        pub limit_display: String,
        pub pct: i64,
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
            Self {
                label,
                current,
                limit_display: if unlimited {
                    "∞".to_string()
                } else {
                    limit.to_string()
                },
                pct,
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
        pub max_logo_size_kb: i32,
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
                UsageBar::new("Targets", u.targets, p.max_targets),
                UsageBar::new("Members", u.members, p.max_members),
                UsageBar::new(
                    "Public components",
                    u.public_components,
                    p.max_public_components,
                ),
                UsageBar::new(
                    "Pending invitations",
                    u.pending_invitations,
                    p.max_pending_invitations,
                ),
                UsageBar::new(
                    "Maintenance windows",
                    u.maintenance_windows,
                    p.max_maintenance_windows,
                ),
                UsageBar::new(
                    "Notification channels",
                    u.notification_channels,
                    p.max_notification_channels,
                ),
            ],
            min_check_interval_secs: p.min_check_interval_secs,
            retention_days: p.retention_days,
            max_logo_size_kb: p.max_logo_size_bytes / 1024,
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
                retention_days: 30,
                max_logo_size_kb: 200,
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
            assert!(html.contains("Plan:"));
            assert!(html.contains("Free"));
            // Bounded bar shows the count and a width proportional to usage.
            assert!(html.contains("7 / 10"));
            assert!(html.contains("width: 70%"));
            // Unlimited (self-host) cap renders ∞, not i32::MAX, at 0%.
            assert!(html.contains("1 / ∞"));
            assert!(html.contains("width: 0%"));
            assert!(html.contains("60 seconds"));
            assert!(html.contains(r#"href="mailto:upgrade@your-domain.com""#));
        }

        #[test]
        fn account_page_renders_privacy_and_sessions_sections() {
            let html = AccountPage {
                active_tab: super::super::TAB_ACCOUNT,
                email: "alice@example.com".into(),
                provider: "GitHub".into(),
                joined: "2026-02-14 09:00 UTC".into(),
                last_seen: "2026-05-16 12:00 UTC".into(),
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
            assert!(html.contains("Active sessions"));
            assert!(html.contains(r#"hx-get="/web/partials/settings/sessions""#));
            assert!(html.contains("logout-all"));
        }

        #[test]
        fn api_tokens_page_renders_create_form_and_partial_hook() {
            let html = ApiTokensPage {
                active_tab: super::super::TAB_SETTINGS,
            }
            .render()
            .unwrap();
            assert!(html.contains("API tokens"));
            assert!(html.contains(r#"hx-post="/api/v1/me/api-tokens""#));
            assert!(html.contains(r#"hx-get="/web/partials/settings/api-tokens""#));
        }

        #[test]
        fn sessions_partial_renders_empty_state() {
            let html = SessionsPartial { sessions: vec![] }.render().unwrap();
            assert!(html.contains("No active sessions"));
            // Partial must not include the page chrome — it's swapped in via HTMX.
            assert!(!html.contains("<!doctype html>"));
        }

        #[test]
        fn sessions_partial_marks_current_session() {
            let html = SessionsPartial {
                sessions: vec![SessionRow {
                    id: "abc".into(),
                    created: "now".into(),
                    last_used: "now".into(),
                    expires: "soon".into(),
                    ip_short: "deadbeefcafe".into(),
                    is_current: true,
                }],
            }
            .render()
            .unwrap();
            assert!(html.contains("This device"));
            assert!(!html.contains("hx-delete"));
        }

        #[test]
        fn tokens_partial_renders_revoke_when_present() {
            let html = TokensPartial {
                tokens: vec![TokenRow {
                    id: "tok-1".into(),
                    name: "CI".into(),
                    prefix: "sm_live_aaaaaaaa".into(),
                    created: "now".into(),
                    last_used: "never".into(),
                }],
            }
            .render()
            .unwrap();
            assert!(html.contains("CI"));
            assert!(html.contains("sm_live_aaaaaaaa"));
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
        assert!(html.contains("Continue with GitHub"));
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
        assert!(!html.contains("Continue with GitHub"));
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
            current_name: "personal-quiet-koala".into(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Welcome, Alice"));
        assert!(html.contains(r#"value="personal-quiet-koala""#));
        assert!(html.contains(r#"hx-patch="/api/v1/orgs/00000000-0000-0000-0000-000000000001""#));
        assert!(html.contains(r#""X-Requested-With":"status-monitor""#));
    }

    #[test]
    fn recover_page_renders_confirm_when_token_present() {
        let html = RecoverAccountPage {
            active_tab: TAB_RECOVER,
            token: "tok-abc".into(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Recover your account"));
        assert!(html.contains(r#"hx-post="/api/v1/auth/recover-account""#));
        assert!(html.contains(r#"hx-ext="json-enc""#));
        assert!(html.contains(r#"value="tok-abc""#));
        // Signed-out page: the authenticated header nav must be suppressed.
        assert!(!html.contains("Log out"));
    }

    #[test]
    fn recover_page_blank_token_shows_invalid_state() {
        let html = RecoverAccountPage {
            active_tab: TAB_RECOVER,
            token: String::new(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Recovery link invalid"));
        assert!(!html.contains("hx-post"));
    }

    #[test]
    fn display_name_from_email() {
        assert_eq!(display_name_for("alice@example.com"), "Alice");
        assert_eq!(display_name_for("alice.smith@example.com"), "Alice");
        assert_eq!(display_name_for("bob_jones@example.com"), "Bob");
        assert_eq!(display_name_for("@example.com"), "There");
    }
}
