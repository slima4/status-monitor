//! Live-PG end-to-end coverage for the per-org public status surface served
//! at `{slug}.{base_domain}`. Skipped by default; runs under
//! `--include-ignored` once `DATABASE_URL` is set.
//!
//! Two layers of assertion:
//!
//!  * Storage gate: the public-path resolver `find_public_status_org_by_slug`
//!    filters `public_status_enabled = true`, whereas the authenticated
//!    `find_id_by_slug` does not. Swapping them would publish every org's
//!    page regardless of opt-in.
//!  * HTTP gate: the host-aware `StatusPageOrg` extractor admits a request
//!    only for an enabled slug. A blocked request is a 404 (extractor
//!    rejection); an admitted one renders the page (200). The 200-vs-404
//!    split proves the extractor did the gating. Disabling via the operator
//!    storage path flips a previously-served slug back to 404.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{make_user, unique_slug};
use status_monitor::domain::{OrgId, PublicOrgBranding, UserId};
use status_monitor::storage::orgs::{find_id_by_slug, find_public_status_org_by_slug};
use status_monitor::storage::{create_org_with_owner, update_public_branding};
use tower::ServiceExt;

/// The `public_status.base_domain` config value. Status pages live at
/// `{slug}.{BASE_DOMAIN}` (apex-wildcard shape); see [`status_host`], the
/// only place that contract is spelled so a host can never silently drift
/// from what the parser expects.
const BASE_DOMAIN: &str = "test.local";

/// Build the public-status `Host` for a slug, matching the production
/// `{slug}.{base_domain}` contract the host parser enforces.
fn status_host(slug: &str) -> String {
    format!("{slug}.{BASE_DOMAIN}")
}

/// Create an org owned by a fresh user and set its public-status opt-in
/// through the same storage call the operator endpoint uses. Returns the
/// org plus its owner so callers can flip the opt-in again later.
async fn seed_org(pool: &sqlx::PgPool, slug: &str, enabled: bool) -> (OrgId, UserId) {
    let user = make_user(pool, "subdomain").await;
    let org = create_org_with_owner(pool, user, slug, slug, 100)
        .await
        .expect("create org")
        .expect("slug not taken");
    set_enabled(pool, org.id, user, enabled).await;
    (org.id, user)
}

async fn set_enabled(pool: &sqlx::PgPool, org: OrgId, actor: UserId, enabled: bool) {
    let branding = PublicOrgBranding {
        public_status_enabled: enabled,
        ..PublicOrgBranding::default()
    };
    let updated = update_public_branding(pool, org, actor, &branding)
        .await
        .expect("update branding");
    assert!(updated, "org must exist for branding update");
}

/// `GET /status` against the SaaS-subdomain app with an explicit `Host`.
async fn get_status(app: &axum::Router, host: Option<&str>) -> StatusCode {
    get_path(app, "/status", host).await
}

/// `GET <path>` against the SaaS-subdomain app with an explicit `Host`.
async fn get_path(app: &axum::Router, path: &str, host: Option<&str>) -> StatusCode {
    let mut req = Request::builder().uri(path);
    if let Some(h) = host {
        req = req.header("host", h);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .expect("oneshot")
        .status()
}

fn saas_subdomain(cfg: &mut status_monitor::config::AppConfig) {
    cfg.tenancy.enabled = true;
    cfg.tenancy.subdomain_public_routes = true;
    cfg.tenancy.path_based_public_routes = false;
    cfg.public_status.base_domain = BASE_DOMAIN.into();
    cfg.auth.session.cookie_domain = String::new();
}

#[tokio::test]
#[ignore]
async fn public_lookup_filters_disabled_orgs_but_authed_lookup_does_not() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let on = unique_slug("on");
    let off = unique_slug("off");
    seed_org(&pool, &on, true).await;
    seed_org(&pool, &off, false).await;

    // The public path sees only the opted-in org.
    assert!(
        find_public_status_org_by_slug(&pool, &on)
            .await
            .unwrap()
            .is_some(),
        "enabled org visible to public lookup"
    );
    assert!(
        find_public_status_org_by_slug(&pool, &off)
            .await
            .unwrap()
            .is_none(),
        "disabled org hidden from public lookup"
    );
    assert!(
        find_public_status_org_by_slug(&pool, &unique_slug("ghost"))
            .await
            .unwrap()
            .is_none(),
        "nonexistent slug → None"
    );

    // The authenticated lookup ignores the opt-in flag — proves the gating
    // lives in the public helper, not in slug existence. Substituting it on
    // the public path would expose the disabled org.
    assert!(find_id_by_slug(&pool, &on).await.unwrap().is_some());
    assert!(
        find_id_by_slug(&pool, &off).await.unwrap().is_some(),
        "authed lookup still resolves the disabled org"
    );
}

#[tokio::test]
#[ignore]
async fn subdomain_status_page_gates_on_enabled_and_slug_shape() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let on = unique_slug("on");
    let off = unique_slug("off");
    seed_org(&pool, &on, true).await;
    seed_org(&pool, &off, false).await;

    let (app, _default) = common::build_test_app_with_pg(pool, saas_subdomain).await;

    // Enabled slug: extractor admits the request and the page renders (200).
    // The blocked shapes below all 404, so 200-vs-404 isolates the gate.
    let admitted = get_status(&app, Some(&status_host(&on))).await;
    assert_eq!(
        admitted,
        StatusCode::OK,
        "enabled slug must render its page"
    );

    // Every blocked shape collapses to 404 at the extractor.
    for (host, why) in [
        (Some(status_host(&off)), "disabled org"),
        (Some(status_host(&unique_slug("ghost"))), "unknown slug"),
        (Some(format!("a.b.{BASE_DOMAIN}")), "deeper subdomain"),
        (Some(BASE_DOMAIN.to_string()), "bare base domain"),
    ] {
        assert_eq!(
            get_status(&app, host.as_deref()).await,
            StatusCode::NOT_FOUND,
            "{why} must 404"
        );
    }
    assert_eq!(
        get_status(&app, None).await,
        StatusCode::NOT_FOUND,
        "missing Host header must 404"
    );
}

#[tokio::test]
#[ignore]
async fn subdomain_root_serves_public_page() {
    // `/` on a `{slug}.{base_domain}` host renders the public page just like
    // `/status` does — competitor parity (Statuspage/BetterStack/Instatus
    // all serve at apex). The route stays at `/status`; the request URI is
    // rewritten before the router matches.
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let slug = unique_slug("root");
    seed_org(&pool, &slug, true).await;
    let (app, _default) = common::build_test_app_with_pg(pool, saas_subdomain).await;

    let host = status_host(&slug);
    assert_eq!(
        get_path(&app, "/", Some(&host)).await,
        StatusCode::OK,
        "subdomain `/` must serve the public page"
    );
    assert_eq!(
        get_path(&app, "/?utm_source=email", Some(&host)).await,
        StatusCode::OK,
        "query string must survive the rewrite"
    );
    // Blocked host shapes don't trigger the rewrite — `/` falls through to
    // the dashboard route, which requires auth and never returns 200 here.
    assert_ne!(
        get_path(&app, "/", Some(&format!("a.b.{BASE_DOMAIN}"))).await,
        StatusCode::OK,
        "deeper subdomain must NOT be treated as a public surface"
    );
    assert_ne!(
        get_path(&app, "/", Some(BASE_DOMAIN)).await,
        StatusCode::OK,
        "bare base domain must NOT be treated as a public surface"
    );
}

#[tokio::test]
#[ignore]
async fn root_on_non_subdomain_host_falls_through_to_dashboard() {
    // Hosts that don't parse as `{slug}.{base_domain}` (bare base, deeper
    // subdomain, missing Host) must NOT be treated as the public surface —
    // they should hit the operator dashboard. With no session cookie, that
    // branch redirects to /login (303), proving the dispatcher reached the
    // dashboard path and not `public_status::index` (which would 404).
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (app, _default) = common::build_test_app_with_pg(pool, saas_subdomain).await;

    for (host, why) in [
        (Some(BASE_DOMAIN.to_string()), "bare base domain"),
        (Some(format!("a.b.{BASE_DOMAIN}")), "deeper subdomain"),
        (None, "missing Host header"),
    ] {
        assert_eq!(
            get_path(&app, "/", host.as_deref()).await,
            StatusCode::SEE_OTHER,
            "operator-style `/` ({why}) must redirect to /login, not 404"
        );
    }
}

#[tokio::test]
#[ignore]
async fn disabling_via_operator_path_takes_the_page_offline() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let slug = unique_slug("flip");
    let (org, user) = seed_org(&pool, &slug, true).await;

    let (app, _default) = common::build_test_app_with_pg(pool.clone(), saas_subdomain).await;
    let host = status_host(&slug);

    assert_eq!(
        get_status(&app, Some(&host)).await,
        StatusCode::OK,
        "enabled org renders its page"
    );

    // Operator turns the page off through the real storage path.
    set_enabled(&pool, org, user, false).await;

    assert_eq!(
        get_status(&app, Some(&host)).await,
        StatusCode::NOT_FOUND,
        "disabled org no longer resolves on its subdomain"
    );
}
