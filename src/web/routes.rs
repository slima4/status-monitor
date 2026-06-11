use axum::Router;
use axum::routing::{get, post};
use tower_cookies::CookieManagerLayer;

use crate::api::public_routes_active;
use crate::app::AppState;
use crate::web::{assets, error, views};

/// Builds the UI router with state applied. The public-status pages
/// (`/status`, `/status/incidents/{id}`) are mounted only when
/// [`public_routes_active`] is true; the org they render is resolved
/// per-request by the host-aware `StatusPageOrg` extractor (subdomain →
/// that tenant; self-host → the default org). `/` runs a host-aware
/// dispatcher ([`views::dashboard::root`]) so on the SaaS subdomain
/// surface each org's page lives at the apex of its host (industry
/// parity); the operator dashboard keeps `/` on its own host.
pub fn routes(state: AppState) -> Router {
    let cfg = &state.cfg;
    crate::web::filters::set_escalation_ui(cfg.escalation.enabled);
    let mut r = Router::new()
        .route("/", get(views::dashboard::root))
        .route("/targets", get(views::targets_list::index))
        .route("/targets/new", get(views::targets_form::new_form))
        .route("/targets/{id}", get(views::targets_detail::index))
        .route(
            "/targets/{id}/incidents",
            get(views::targets_detail::incidents),
        )
        .route("/targets/{id}/edit", get(views::targets_form::edit_form))
        .route("/incidents", get(views::incidents::list))
        .route(
            "/web/partials/incidents",
            get(views::incidents::list_partial),
        )
        .route("/incidents/declare", get(views::incidents::declare_form))
        .route("/incidents/reports", get(views::incidents::reports))
        .route("/incidents/{id}", get(views::incidents::detail))
        .route(
            "/incidents/{id}/postmortem",
            get(views::incidents::postmortem_form),
        )
        .route("/web/targets/list", get(views::targets_list::list_partial))
        .route(
            "/web/partials/targets/{id}/live",
            get(views::targets_detail::live_partial),
        )
        // Public capability links: a token grants read-only access to one
        // monitor's detail view. Unauthenticated by design; the token resolves
        // to its org. Always mounted (not gated on public_routes_active) — share
        // links live on the operator app host, not the per-tenant status hosts.
        .route("/m/{token}", get(views::share::detail))
        .route("/m/{token}/incidents", get(views::share::incidents))
        .route("/m/{token}/live", get(views::share::live_partial))
        .route("/m/{token}/latency", get(views::share::latency))
        .route("/m/{token}/results", get(views::share::results))
        .route(
            "/web/partials/dashboard",
            get(views::dashboard::table_partial),
        )
        .route("/login", get(views::auth::login))
        .route("/onboarding/org", get(views::auth::onboarding_org))
        .route(
            "/settings/account",
            get(views::auth::settings::account_page),
        )
        .route(
            "/settings/sessions",
            get(views::auth::settings::sessions_page),
        )
        .route(
            "/settings/api-tokens",
            get(views::auth::settings::api_tokens_page),
        )
        .route("/settings/usage", get(views::auth::settings::usage_page))
        .route("/settings/pages", get(views::pages::pages_list))
        .route("/settings/pages/{id}", get(views::pages::page_editor))
        .route(
            "/web/partials/settings/pages",
            get(views::pages::pages_partial),
        )
        .route(
            "/settings/notifications",
            get(views::notification_channels::index),
        )
        .route(
            "/settings/notifications/new",
            get(views::notification_channels::new_form),
        )
        .route(
            "/settings/notifications/{id}/edit",
            get(views::notification_channels::edit_form),
        )
        .route(
            "/web/partials/settings/notifications",
            get(views::notification_channels::list_partial),
        )
        .route(
            "/web/partials/settings/sessions",
            get(views::auth::settings::sessions_partial),
        )
        .route(
            "/web/partials/settings/api-tokens",
            get(views::auth::settings::api_tokens_partial),
        )
        .route("/terms", get(views::legal::terms))
        .route("/privacy", get(views::legal::privacy))
        .route("/cookies", get(views::legal::cookies))
        .route("/impressum", get(views::legal::impressum))
        .route("/abuse-policy", get(views::legal::abuse_policy))
        .route("/security-policy", get(views::legal::security_policy))
        .route("/licenses", get(views::legal::licenses))
        .route("/.well-known/security.txt", get(views::legal::security_txt));

    // Team-paging surfaces — mounted only when escalation is enabled, so a
    // single-responder deployment never exposes policy/schedule config that
    // nothing consumes.
    if cfg.escalation.enabled {
        r = r
            .route("/settings/escalation", get(views::escalation::index))
            .route("/settings/escalation/new", get(views::escalation::new_form))
            .route(
                "/settings/escalation/{id}/edit",
                get(views::escalation::edit_form),
            )
            .route(
                "/web/partials/settings/escalation",
                get(views::escalation::list_partial),
            )
            .route("/settings/on-call", get(views::on_call::index))
            .route("/settings/on-call/new", get(views::on_call::new_form))
            .route(
                "/settings/on-call/{id}/edit",
                get(views::on_call::edit_form),
            )
            .route(
                "/web/partials/settings/on-call",
                get(views::on_call::list_partial),
            );
    }

    // Central Telegram bot receiver. Mounted only when an operator bot token
    // is configured; otherwise the feature is absent. Lives on the app host
    // alongside the share links, authenticated by the per-update secret token.
    if cfg.telegram.enabled() {
        r = r.route(
            crate::telegram::WEBHOOK_PATH,
            post(views::telegram::webhook),
        );
    }

    if public_routes_active(cfg) {
        r = r
            .route("/status", get(views::public_status::index))
            .route("/status/incidents", get(views::public_status::archive))
            .route(
                "/status/incidents/{id}",
                get(views::public_status::incident),
            )
            .route(
                views::public_status::LOGO_ROUTE,
                get(views::public_status::logo),
            );
    }

    // OAuth 2.1 authorization-server + RFC 9728 resource metadata for the MCP
    // connector. Merged here so the endpoints inherit cookies (the consent flow
    // reads the session) + CSRF + the cross-cutting layers. Gated internally on
    // mcp config.
    r = r.merge(crate::oauth::routes(cfg));

    assets::mount_static(r)
        .fallback(error::not_found)
        .layer(CookieManagerLayer::new())
        .with_state(state)
}
