//! Scope enforcement for API tokens: a read-only token can GET targets but is
//! denied writes with `INSUFFICIENT_SCOPE`; a full-access token can do both.
//!
//! Live-PG ignored: needs a Postgres pool. The scope check is in the
//! `Authorized<R>` extractor, in front of the unchanged per-org-id store.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{build_saas_router_with_pg_targets, make_user, unique_slug};
use serde_json::json;
use tower::ServiceExt;
use uptimepage::auth::api_tokens;
use uptimepage::storage::create_org_with_owner;

const PREFIX_LEN: usize = 16;

fn target_body() -> String {
    json!({
        "name": "scoped",
        "check": {
            "type": "http",
            "url": "https://example.com/",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true
        },
        "interval": 60,
        "enabled": true,
        "tags": []
    })
    .to_string()
}

async fn send(
    router: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    org: &str,
    body: Option<String>,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("x-uptimepage-org", org);
    let body = match body {
        Some(j) => {
            builder = builder.header("content-type", "application/json");
            Body::from(j)
        }
        None => Body::empty(),
    };
    let resp = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn read_only_token_gets_but_cannot_write_targets() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };

    let user = make_user(&pool, "scope").await;
    let slug = unique_slug("scope");
    create_org_with_owner(&pool, user, &slug, "Scope", 3)
        .await
        .unwrap()
        .expect("org");

    // Read-only token: create (defaults to full_access) then narrow the scopes
    // directly — the scoped-creation API arrives in a later phase.
    let ro = api_tokens::create(&pool, user, "ro", PREFIX_LEN, 1000)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE api_tokens SET scopes = '["targets:read"]'::jsonb WHERE id = $1"#)
        .bind(ro.id)
        .execute(&pool)
        .await
        .unwrap();

    // Full-access token (default scopes).
    let fa = api_tokens::create(&pool, user, "fa", PREFIX_LEN, 1000)
        .await
        .unwrap();

    // Token scoped to a DIFFERENT resource — proves the read gate discriminates
    // by resource, not just read-vs-write.
    let wrong = api_tokens::create(&pool, user, "wrong", PREFIX_LEN, 1000)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE api_tokens SET scopes = '["channels:read"]'::jsonb WHERE id = $1"#)
        .bind(wrong.id)
        .execute(&pool)
        .await
        .unwrap();

    let router = build_saas_router_with_pg_targets(pool.clone()).await;

    // Read-only token: listing is allowed.
    let (status, _) = send(&router, "GET", "/api/v1/targets", &ro.token, &slug, None).await;
    assert_eq!(status, StatusCode::OK, "read-only token must GET targets");

    // Channels-only token: cannot even GET targets.
    let (status, body) = send(&router, "GET", "/api/v1/targets", &wrong.token, &slug, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "channels-only token must not read targets"
    );
    assert!(
        body.contains("INSUFFICIENT_SCOPE"),
        "expected INSUFFICIENT_SCOPE, got: {body}"
    );

    // The targets-only token also lacks channels:read — cross-resource gate.
    let (status, body) = send(
        &router,
        "GET",
        "/api/v1/notification-channels",
        &ro.token,
        &slug,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "targets-only token must not read channels"
    );
    assert!(
        body.contains("INSUFFICIENT_SCOPE"),
        "expected INSUFFICIENT_SCOPE, got: {body}"
    );

    // Read-only token: creating is denied with INSUFFICIENT_SCOPE.
    let (status, body) = send(
        &router,
        "POST",
        "/api/v1/targets",
        &ro.token,
        &slug,
        Some(target_body()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "read-only token must not create targets"
    );
    assert!(
        body.contains("INSUFFICIENT_SCOPE"),
        "expected INSUFFICIENT_SCOPE, got: {body}"
    );

    // Full-access token: creating succeeds.
    let (status, body) = send(
        &router,
        "POST",
        "/api/v1/targets",
        &fa.token,
        &slug,
        Some(target_body()),
    )
    .await;
    assert!(
        status.is_success(),
        "full-access token must create targets, got {status}: {body}"
    );
}
