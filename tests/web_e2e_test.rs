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

/// Docs live on the marketing host, which an app-only deployment does not
/// run, so the app must link out absolutely rather than to its own `/docs`
/// (that path is the Swagger UI here).
#[tokio::test]
async fn app_chrome_links_out_to_the_docs() {
    let resp = app()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let html = body_text(resp).await;
    // The account popover renders twice (desktop + mobile), plus the footer.
    assert_eq!(
        html.matches(r#"href="https://uptimepage.dev/docs""#)
            .count(),
        3,
        "expected the docs link in both nav popovers and the footer"
    );
    assert!(
        !html.contains(r#"href="/docs""#),
        "a relative /docs on the app host is the Swagger UI, not the docs"
    );
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
    assert!(html.contains(r#"name="check_type" value="http" checked"#));
    assert!(html.contains(r#"name="check_type" value="heartbeat""#));
    // Credentials are supplied via headers with secret variables, not inline auth fields.
    assert!(
        html.contains("data-var-auth-picker"),
        "secret-variable auth picker present"
    );
    assert!(
        html.contains("variable_helpers"),
        "insert-variable helper script linked"
    );
}

/// A monitor bound to nothing pages nobody, so a sole channel is ticked. A
/// second one makes the right routing ambiguous.
#[tokio::test]
async fn create_form_ticks_a_sole_channel_but_not_a_pair() {
    let router = app();
    let add_channel = async |name: &str| {
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/v1/notification-channels")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "name": name,
                            "config": { "type": "email", "to": format!("{name}@example.com") }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "create channel {name}");
    };

    // Scoped to the channel inputs: verify_tls and the interval rail also
    // render checked.
    let channel_boxes = |html: &str| -> (usize, usize) {
        let mut offered = 0;
        let mut ticked = 0;
        for (i, _) in html.match_indices("data-channel-select") {
            let tail = &html[i..];
            let tag = &tail[..tail.find('>').unwrap_or(tail.len())];
            offered += 1;
            ticked += usize::from(tag.contains(" checked"));
        }
        (offered, ticked)
    };

    add_channel("solo").await;
    let resp = router
        .clone()
        .oneshot(Request::get("/targets/new").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        channel_boxes(&body_text(resp).await),
        (1, 1),
        "the only channel is offered and preselected"
    );

    add_channel("second").await;
    let resp = router
        .clone()
        .oneshot(Request::get("/targets/new").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        channel_boxes(&body_text(resp).await),
        (2, 0),
        "with two channels the form must not guess"
    );
}

#[tokio::test]
async fn edit_form_renders_existing_target_without_leaking_credentials() {
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
    assert!(html.contains(r#"value="redacted-edit-target""#));
    assert!(html.contains(r#"data-method="PATCH""#));
    assert!(html.contains(r#"data-mode="edit""#));
    // The kind is fixed once the monitor exists: rail is inert, nothing to submit with.
    assert!(!html.contains("data-check-card"));
    assert!(html.contains(r#"<input type="hidden" name="check_type" value="http">"#));
    // Stored credentials must never reach the form HTML.
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

/// Gaps are named while they are gaps, and stop being named once watched.
#[tokio::test]
async fn detail_offers_uncovered_checks_for_the_host() {
    let router = app();
    let id = create_http_target(&router, "coverage-target").await;

    let detail = async || {
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
        body_text(resp).await
    };

    let html = detail().await;
    assert!(html.contains("also worth watching"));
    // Matched up to the query separator: the `&` is escaped in rendered HTML
    // and the exact entity is the templating engine's call.
    for href in [
        r#"href="/targets/new?kind=tls_cert"#,
        r#"href="/targets/new?kind=domain_expiry"#,
        r#"href="/targets/new?kind=dns"#,
    ] {
        assert!(html.contains(href), "missing suggestion {href}");
    }
    assert_eq!(
        html.matches("host=example.com").count(),
        3,
        "every suggestion points at the monitor's host"
    );

    // Cover one of them; the panel drops that row and keeps the rest.
    let cert = json!({
        "name": "cert",
        "interval": 3600,
        "enabled": true,
        "check": {
            "type": "tls_cert",
            "host": "example.com",
            "port": 443,
            "warn_days": 30,
            "critical_days": 7,
            "timeout": 5000
        }
    });
    let resp = router
        .clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(cert.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create cert monitor");

    let html = detail().await;
    assert!(
        !html.contains(r#"href="/targets/new?kind=tls_cert"#),
        "a covered check must stop being suggested"
    );
    assert!(html.contains(r#"href="/targets/new?kind=dns"#));
}

#[tokio::test]
async fn create_form_prefills_from_a_coverage_link() {
    let resp = app()
        .oneshot(
            Request::get("/targets/new?kind=domain_expiry&host=acme.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains(r#"value="acme.com""#), "host prefilled");
    assert!(html.contains(r#"value="acme.com domain expiry""#), "named");
    assert!(html.contains(r#"name="check_type" value="domain_expiry" checked"#));
    // Opens on the suggested cadence for the kind, not on the 60s default.
    assert!(html.contains(r#"data-interval="86400""#));

    // A hand-edited kind is ignored rather than rejected.
    let resp = app()
        .oneshot(
            Request::get("/targets/new?kind=nonsense&host=acme.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!body_text(resp).await.contains(r#"value="acme.com""#));
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

#[tokio::test]
async fn variables_page_and_partial_render_with_secret_redacted() {
    let router = app();

    let resp = router
        .clone()
        .oneshot(
            Request::get("/settings/variables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("add variable"), "create form must render");

    for body in [
        json!({ "key": "base_url", "is_secret": false, "value": "api.example.com" }),
        json!({ "key": "api_key", "is_secret": true, "value": "sk-super-secret-9f3a" }),
    ] {
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/v1/variables")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "create variable");
    }

    let resp = router
        .clone()
        .oneshot(
            Request::get("/web/partials/settings/variables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(
        html.contains("base_url") && html.contains("api.example.com"),
        "plain value visible"
    );
    assert!(html.contains("api_key"), "secret key listed");
    assert!(
        !html.contains("sk-super-secret-9f3a"),
        "secret value must never reach the page"
    );
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
        body.contains("Contact: mailto:security@uptimepage.dev"),
        "security.txt needs a Contact field"
    );
    assert!(
        body.contains("Expires: 2027-12-31T23:59:59.000Z"),
        "security.txt needs an Expires field"
    );
}

#[tokio::test]
async fn channel_form_gates_one_tap_telegram_on_central_bot() {
    // Default config has no bot token: only the BYO "telegram bot" card.
    let resp = app()
        .oneshot(
            Request::get("/settings/notifications/new")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(!html.contains(r#"value="telegram_app""#));
    assert!(html.contains("telegram bot"));

    let with_bot = build_test_app_with_web_and_owner(|cfg| {
        cfg.telegram.bot_token = "123:abc".to_string().into();
        cfg.telegram.bot_username = "uptimepagebot".into();
        cfg.telegram.webhook_secret = "0123456789abcdef0123456789abcdef".to_string().into();
    });
    let resp = with_bot
        .oneshot(
            Request::get("/settings/notifications/new")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains(r#"value="telegram_app""#), "one-tap card");
    assert!(html.contains("data-tga-connect"), "connect button");
    assert!(html.contains("data-tga-group"), "group destination toggle");
    assert!(html.contains("one-tap chat link"));
}

/// The host serving the API points agents at the catalog rather than 404ing
/// on the path they are specified to look at.
#[tokio::test]
async fn api_catalog_redirects_to_the_marketing_host() {
    let app = build_test_app_with_web_and_owner(|cfg| {
        cfg.marketing.enabled = true;
        cfg.marketing.canonical_origin = "https://example.test".into();
    });
    let resp = app
        .oneshot(
            Request::get("/.well-known/api-catalog")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        resp.headers().get(header::LOCATION).unwrap(),
        "https://example.test/.well-known/api-catalog"
    );
}
