//! End-to-end coverage for the server-rendered UI.
//!
//! Exercises every web route via tower::ServiceExt::oneshot, asserting
//! status code, content-type, and the structural anchors that the JS
//! layer relies on (HTMX hooks, chart data-endpoints, form data-action,
//! credential redaction sentinels).

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{build_test_app_with_web, build_test_app_with_web_and_owner};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> axum::Router {
    build_test_app_with_web_and_owner(|_| {})
}

async fn body_text(resp: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(resp.into_body(), 4 << 20).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn html_ct(resp: &axum::http::Response<Body>) -> &str {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

async fn create_http_target(router: &axum::Router, name: &str) -> String {
    create_http_target_with_alerts(router, name, json!([])).await
}

async fn create_http_target_with_alerts(
    router: &axum::Router,
    name: &str,
    alerts: Value,
) -> String {
    let body = json!({
        "name": name,
        "interval": 60,
        "enabled": true,
        "tags": ["e2e"],
        "alerts": alerts,
        "check": {
            "type": "http",
            "url": "https://example.com/",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true,
            "basic_auth": ["alice", "s3cret"],
            "bearer_token": "tok-abc"
        }
    });
    let resp = router
        .clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create target");
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().expect("id").to_string()
}

#[tokio::test]
async fn dashboard_renders_with_kpi_cards_and_chart_anchors() {
    let resp = app()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(html_ct(&resp).starts_with("text/html"));
    let html = body_text(resp).await;
    assert!(html.contains("Dashboard"));
    assert!(html.contains("nothing to watch yet."));
    assert!(html.contains(r#"href="/targets/new""#));
}

#[tokio::test]
async fn dashboard_partial_returns_chrome_free_fragment() {
    let resp = app()
        .oneshot(
            Request::get("/web/partials/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(!html.contains("<!doctype html>"));
    assert!(!html.contains("<nav"));
}

#[tokio::test]
async fn targets_list_empty_org_renders_onboarding_card() {
    // Default test app has zero monitors → /targets renders the
    // onboarding empty state instead of filters + table chrome.
    let resp = app()
        .oneshot(Request::get("/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("Monitors"));
    assert!(html.contains("nothing to watch yet."));
    assert!(html.contains("add your first monitor"));
    assert!(!html.contains(r#"id="targets-filter""#));
}

#[tokio::test]
async fn targets_list_partial_returns_tbody_only() {
    let resp = app()
        .oneshot(
            Request::get("/web/targets/list?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(!html.contains("<!doctype html>"));
    assert!(!html.contains("<nav"));
    assert!(html.contains(r#"id="target-rows""#));
}

#[tokio::test]
async fn new_target_form_renders_create_mode() {
    let resp = app()
        .oneshot(Request::get("/targets/new").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("New monitor"));
    assert!(html.contains(r#"data-action="/api/v1/targets""#));
    assert!(html.contains(r#"data-method="POST""#));
    assert!(html.contains(r#"data-mode="create""#));
    assert!(html.contains(r#"data-auth-field="basic""#));
    assert!(html.contains(r#"data-initial-mode="create""#));
    assert!(html.contains("set credentials"));
    assert!(html.contains("set token"));
}

#[tokio::test]
async fn edit_form_shows_redacted_auth_state_for_existing_target() {
    let router = app();
    let id = create_http_target(&router, "redacted-edit-target").await;

    let resp = router
        .clone()
        .oneshot(
            Request::get(format!("/targets/{id}/edit"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("edit redacted-edit-target"));
    assert!(html.contains(r#"data-method="PATCH""#));
    assert!(html.contains(r#"data-mode="edit""#));
    assert!(html.contains(r#"data-initial-mode="redacted""#));
    assert!(html.contains("replace credentials"));
    assert!(html.contains("replace token"));
    // Real values must NEVER appear in the HTML; only the sentinel does.
    assert!(!html.contains("s3cret"));
    assert!(!html.contains("tok-abc"));
}

#[tokio::test]
async fn target_detail_renders_charts_and_range_nav() {
    let router = app();
    let id = create_http_target(&router, "detail-target").await;

    let resp = router
        .clone()
        .oneshot(
            Request::get(format!("/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("detail-target"));
    assert!(html.contains(r#"aria-label="Time range""#));
    for key in ["1h", "24h", "7d", "30d"] {
        assert!(
            html.contains(&format!("?range={key}")),
            "missing range {key}"
        );
    }
    assert!(html.contains(r#"id="latency-chart""#));
    assert!(html.contains(r#"id="breakdown-chart""#));
    assert!(html.contains("/api/v1/targets/"));
    assert!(html.contains("/static/js/charts/detail_charts.js"));
}

#[tokio::test]
async fn nonexistent_target_detail_returns_html_404() {
    let resp = app()
        .oneshot(
            Request::get("/targets/00000000-0000-0000-0000-000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(html_ct(&resp).starts_with("text/html"));
    let html = body_text(resp).await;
    assert!(html.contains("Not Found"));
}

#[tokio::test]
async fn unknown_web_path_returns_404_html() {
    let resp = app()
        .oneshot(
            Request::get("/this-route-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Cache-control is only `immutable` when the URL is version-pinned (`?v=`,
/// the only form the `asset` filter emits). A bare URL — hand-typed or an
/// old bookmark — gets a short revalidating cache so a content change can't
/// be hidden for a year. This is the e2e mirror of the `web::assets` unit
/// tests; the two must not disagree.
#[tokio::test]
async fn static_assets_cache_control_is_honest() {
    let cache_control = |resp: &axum::http::Response<Body>| {
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };

    let bare = app()
        .oneshot(
            Request::get("/static/css/app.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::OK);
    assert_eq!(
        cache_control(&bare),
        "public, max-age=300",
        "bare asset URL must be short-lived, not immutable",
    );

    let versioned = app()
        .oneshot(
            Request::get("/static/css/app.css?v=deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(versioned.status(), StatusCode::OK);
    assert_eq!(
        cache_control(&versioned),
        "public, max-age=31536000, immutable",
        "version-pinned asset URL must be immutable",
    );
}

#[tokio::test]
async fn settings_account_redirects_to_login_when_unauthenticated() {
    let resp = build_test_app_with_web(|_| {})
        .oneshot(
            Request::get("/settings/account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "unauthenticated /settings/account must redirect, got {}",
        resp.status()
    );
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(loc.starts_with("/login"), "redirect target was {loc}");
}

/// Operator HTML pages must redirect an anonymous browser to `/login`.
/// The `AuthedBrowser` gate is the single auth model — there is no
/// no-auth pass-through anywhere in the binary.
#[tokio::test]
async fn operator_pages_redirect_to_login_when_unauthenticated_saas() {
    let app = build_test_app_with_web(|_| {});
    for path in [
        "/",
        "/targets",
        "/targets/new",
        "/targets/00000000-0000-0000-0000-000000000000",
        "/targets/00000000-0000-0000-0000-000000000000/edit",
        "/web/targets/list",
        "/web/partials/dashboard",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(
            resp.status().is_redirection(),
            "unauthenticated {path} must redirect, got {}",
            resp.status()
        );
        let loc = resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            loc.starts_with("/login"),
            "{path} redirect target was {loc}"
        );
    }
}

/// Every legal/policy page is public, renders the trusted markdown into the
/// `.legal-doc` shell, and never exposes the authenticated operator nav.
#[tokio::test]
async fn legal_pages_render_public_without_auth_nav() {
    let cases = [
        ("/terms", "Terms of Service"),
        ("/privacy", "Privacy Policy"),
        ("/cookies", "Cookie Policy"),
        ("/impressum", "Impressum"),
        ("/abuse-policy", "Abuse Policy"),
        ("/security-policy", "Security Policy"),
    ];
    for (path, heading) in cases {
        let resp = app()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path} should be 200");
        assert!(
            html_ct(&resp).starts_with("text/html"),
            "{path} should be HTML"
        );
        let body = body_text(resp).await;
        assert!(
            body.contains(&format!("<h1>{heading}</h1>")),
            "{path} should render the markdown H1 {heading:?}"
        );
        assert!(
            body.contains(r#"<article class="legal-doc">"#),
            "{path} should use the legal-doc shell"
        );
        // Standalone layout: no operator nav, no logout button.
        assert!(
            !body.contains("hx-post=\"/auth/logout\""),
            "{path} must not expose the authenticated nav"
        );
        // Footer cross-links to the sibling policies.
        assert!(
            body.contains(r#"href="/privacy""#) && body.contains(r#"href="/security-policy""#),
            "{path} should carry the legal footer links"
        );
    }
}

/// The channel edit page splits monitors by alert binding: bound ones are
/// linked in the used-by grid, unbound ones appear only as bind-picker
/// buttons.
#[tokio::test]
async fn channel_edit_page_lists_bound_monitors() {
    let router = app();

    let resp = router
        .clone()
        .oneshot(
            Request::post("/api/v1/notification-channels")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "ops-slack",
                        "config": {
                            "type": "slack",
                            "webhook_url": "https://hooks.slack.com/services/T/B/x"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create channel");
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let ch_id = v["id"].as_str().expect("id").to_string();

    let bound_id =
        create_http_target_with_alerts(&router, "bound-api", json!([{ "channel_id": ch_id }]))
            .await;
    let unbound_id = create_http_target(&router, "unbound-api").await;

    let resp = router
        .clone()
        .oneshot(
            Request::get(format!("/settings/notifications/{ch_id}/edit").as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("bound-api"), "bound monitor must be listed");
    assert!(
        html.contains(format!(r#"href="/targets/{bound_id}/edit""#).as_str()),
        "bound monitor must link to its edit form"
    );
    assert!(
        html.contains(format!(r#"data-bind-monitor data-target-id="{unbound_id}""#).as_str()),
        "unbound monitor must be offered by the bind picker"
    );
    assert!(
        html.contains("unbound-api"),
        "picker cards must show the monitor name"
    );
    assert!(
        !html.contains(format!(r#"href="/targets/{unbound_id}/edit""#).as_str()),
        "unbound monitor must not appear in the used-by grid"
    );
    assert!(html.contains("# bound to"), "header shows the bound count");
}

/// The Privacy Policy uses Markdown tables; table rendering must be on.
#[tokio::test]
async fn privacy_policy_renders_markdown_tables() {
    let resp = app()
        .oneshot(Request::get("/privacy").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = body_text(resp).await;
    assert!(
        body.contains("<table>") && body.contains("Lawful basis"),
        "privacy policy should render its data table"
    );
}

/// RFC 9116: served at the well-known path, `text/plain`, with the
/// mandatory `Contact` and `Expires` fields.
#[tokio::test]
async fn security_txt_served_per_rfc9116() {
    let resp = app()
        .oneshot(
            Request::get("/.well-known/security.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/plain"),
        "security.txt must be text/plain, got {ct}"
    );
    let body = body_text(resp).await;
    assert!(
        body.contains("Contact: mailto:slima4.u8@gmail.com"),
        "security.txt needs a Contact field"
    );
    assert!(
        body.contains("Expires: 2027-12-31T23:59:59.000Z"),
        "security.txt needs an Expires field"
    );
}
