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

/// Org-bound token (P2): pinned to org A, it operates on A with or without the
/// header, and is rejected when the header names a *different* member org.
#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn org_bound_token_is_pinned_to_its_org() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };

    let user = make_user(&pool, "bind").await;
    let slug_a = unique_slug("bind-a");
    let slug_b = unique_slug("bind-b");
    let org_a = create_org_with_owner(&pool, user, &slug_a, "A", 3)
        .await
        .unwrap()
        .expect("org A");
    create_org_with_owner(&pool, user, &slug_b, "B", 3)
        .await
        .unwrap()
        .expect("org B");

    // Full-access token bound to org A.
    let tok = api_tokens::create(&pool, user, "bound", PREFIX_LEN, 1000)
        .await
        .unwrap();
    sqlx::query("UPDATE api_tokens SET org_id = $1 WHERE id = $2")
        .bind(org_a.id.0)
        .bind(tok.id)
        .execute(&pool)
        .await
        .unwrap();

    let router = build_saas_router_with_pg_targets(pool.clone()).await;

    // Header naming the bound org → allowed.
    let (status, _) = send(&router, "GET", "/api/v1/targets", &tok.token, &slug_a, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bound token must act on its own org"
    );

    // Header naming a different member org → 403 ORG_HEADER_MISMATCH.
    let (status, body) = send(&router, "GET", "/api/v1/targets", &tok.token, &slug_b, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "bound token must not address a different org"
    );
    assert!(
        body.contains("ORG_HEADER_MISMATCH"),
        "expected ORG_HEADER_MISMATCH, got: {body}"
    );

    // Seed a target into org A (via the binding) so the no-header read below
    // can prove it resolved to A *specifically*, not just "some valid org".
    let (status, _) = send(
        &router,
        "POST",
        "/api/v1/targets",
        &tok.token,
        &slug_a,
        Some(target_body()),
    )
    .await;
    assert!(
        status.is_success(),
        "bound token must create in its own org"
    );

    // No header → the binding implies org A: the read must return A's target.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/targets")
                .header("authorization", format!("Bearer {}", tok.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "bound token with no header must imply its org"
    );
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("scoped"),
        "no-header read must resolve to org A (its seeded target), got: {body}"
    );
}

/// status_page scope-gating (#1): a `status_page:read` token reads settings; a
/// token without it (targets-only) is denied `INSUFFICIENT_SCOPE`.
#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn status_page_settings_require_status_page_scope() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };

    let user = make_user(&pool, "sp").await;
    let slug = unique_slug("sp");
    let org = create_org_with_owner(&pool, user, &slug, "SP", 3)
        .await
        .unwrap()
        .expect("org");
    let path = format!("/api/v1/orgs/{}/status-page", org.id.0);

    let reader = api_tokens::create(&pool, user, "sp-read", PREFIX_LEN, 1000)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE api_tokens SET scopes = '["status_page:read"]'::jsonb WHERE id = $1"#)
        .bind(reader.id)
        .execute(&pool)
        .await
        .unwrap();

    let wrong = api_tokens::create(&pool, user, "sp-wrong", PREFIX_LEN, 1000)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE api_tokens SET scopes = '["targets:read"]'::jsonb WHERE id = $1"#)
        .bind(wrong.id)
        .execute(&pool)
        .await
        .unwrap();

    let router = build_saas_router_with_pg_targets(pool.clone()).await;

    let (status, body) = send(&router, "GET", &path, &reader.token, &slug, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "status_page:read token must read settings, got: {body}"
    );

    let (status, body) = send(&router, "GET", &path, &wrong.token, &slug, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "token without status_page:read must be denied"
    );
    assert!(
        body.contains("INSUFFICIENT_SCOPE"),
        "expected INSUFFICIENT_SCOPE, got: {body}"
    );

    // Write gate: a read-only status-page token cannot PATCH settings.
    let patch = r#"{"public_status_enabled":false}"#.to_string();
    let (status, body) = send(
        &router,
        "PATCH",
        &path,
        &reader.token,
        &slug,
        Some(patch.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "status_page:read must not write settings"
    );
    assert!(
        body.contains("INSUFFICIENT_SCOPE"),
        "expected INSUFFICIENT_SCOPE on write, got: {body}"
    );

    // A status_page:write token may PATCH (write ⇒ read).
    let writer = api_tokens::create(&pool, user, "sp-write", PREFIX_LEN, 1000)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE api_tokens SET scopes = '["status_page:write"]'::jsonb WHERE id = $1"#)
        .bind(writer.id)
        .execute(&pool)
        .await
        .unwrap();
    let (status, body) = send(&router, "PATCH", &path, &writer.token, &slug, Some(patch)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "status_page:write must update settings, got: {body}"
    );
}
