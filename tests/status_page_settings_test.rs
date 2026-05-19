//! HTTP-level tests for the operator status-page settings endpoints.
//!
//! Regression cover for #25 (a rejected save must name the offending field so
//! the form can highlight it) and #26 (a normal save — including the folded-in
//! logo step — actually succeeds). Walks the real router via `oneshot`, so the
//! extractor wiring, status codes and JSON error contract are all exercised.
//!
//! Live-PG ignored: the owner gate needs a Postgres pool. Run with
//! `DATABASE_URL` set (see CLAUDE.md) — unset is a no-op pass.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    body_json, build_test_app_with_pg, json_request, make_user, unique_slug, with_session,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// 1×1 RGBA PNG — `image::guess_format` accepts it and it is within every
/// dimension/size bound, so `upload_logo` stores it verbatim.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

async fn read(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let st = resp.status();
    (st, body_json(resp).await)
}

/// Owner-of-a-fresh-org router + the org id. Creating the org through the API
/// is what grants the `owner` membership the settings routes require.
async fn owner_org(pool: PgPool) -> (axum::Router, String) {
    let user = make_user(&pool, "sp").await;
    let logo_dir = std::env::temp_dir().join(format!("sp-logo-{}", unique_slug("d")));
    let (router, _) = build_test_app_with_pg(pool, move |cfg| {
        cfg.tenancy.enabled = true;
        cfg.public_status.logo_dir = logo_dir.to_string_lossy().into_owned();
    })
    .await;
    let router = with_session(router, user, None, None);
    let slug = unique_slug("sp");
    let (st, body) = read(
        router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/orgs",
                json!({ "slug": slug, "name": "SP Co" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    (router, body["id"].as_str().unwrap().to_owned())
}

fn patch(org: &str, payload: Value) -> Request<Body> {
    json_request("PATCH", &format!("/api/v1/orgs/{org}/status-page"), payload)
}

// ── #26: a normal save succeeds and round-trips ─────────────────────────────

#[tokio::test]
#[ignore]
async fn valid_save_persists_and_round_trips() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (router, org) = owner_org(pool).await;

    let (st, _) = read(
        router
            .clone()
            .oneshot(patch(
                &org,
                json!({
                    "public_status_enabled": true,
                    "public_display_name": "Acme Public",
                    "public_about": "All good.",
                    "public_brand_color": "#1A2B3C",
                    "public_show_powered_by": false
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = read(
        router
            .oneshot(
                Request::get(format!("/api/v1/orgs/{org}/status-page"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["public_status_enabled"], true);
    assert_eq!(body["public_display_name"], "Acme Public");
    assert_eq!(body["public_brand_color"], "#1a2b3c"); // lower-cased on write
    assert_eq!(body["show_powered_by"], false);
}

/// The form's default state (no display name / about set) must not read as a
/// spurious rejection — the heart of "doesn't work at all" in #26.
#[tokio::test]
#[ignore]
async fn blank_optionals_save_as_cleared_not_rejected() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (router, org) = owner_org(pool).await;
    let (st, _) = read(
        router
            .oneshot(patch(
                &org,
                json!({
                    "public_status_enabled": false,
                    "public_display_name": "",
                    "public_about": "",
                    "public_brand_color": "#3b82f6",
                    "public_show_powered_by": true
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// API clients (and the legacy htmx checkbox encoding) send `"on"` for a
/// checked box — still accepted, never a deserialize 422.
#[tokio::test]
#[ignore]
async fn checkbox_string_encoding_still_accepted() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (router, org) = owner_org(pool).await;
    let (st, _) = read(
        router
            .oneshot(patch(
                &org,
                json!({ "public_status_enabled": "on", "public_brand_color": "#3b82f6" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

// ── #25: a rejected save names the offending field ──────────────────────────

async fn assert_field_rejection(payload: Value, expect_field: &str) {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (router, org) = owner_org(pool).await;
    let (st, body) = read(router.oneshot(patch(&org, payload)).await.unwrap()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], "BRANDING_INVALID");
    assert_eq!(
        body["error"]["field"], expect_field,
        "the form highlights `error.field`; got {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "a human message must accompany the field"
    );
}

#[tokio::test]
#[ignore]
async fn bad_brand_color_points_at_that_field() {
    assert_field_rejection(
        json!({ "public_brand_color": "not-a-color" }),
        "public_brand_color",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn over_long_about_points_at_that_field() {
    assert_field_rejection(
        json!({ "public_brand_color": "#3b82f6", "public_about": "x".repeat(501) }),
        "public_about",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn over_long_display_name_points_at_that_field() {
    assert_field_rejection(
        json!({ "public_brand_color": "#3b82f6", "public_display_name": "y".repeat(81) }),
        "public_display_name",
    )
    .await;
}

// ── #26: the folded-in logo step and the branding save are independent ──────

#[tokio::test]
#[ignore]
async fn logo_upload_then_branding_patch_keeps_logo() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (router, org) = owner_org(pool).await;

    let boundary = "X-SP-BOUNDARY";
    let mut multipart = Vec::new();
    multipart.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"l.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    multipart.extend_from_slice(TINY_PNG);
    multipart.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let (st, body) = read(
        router
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/orgs/{org}/status-page/logo"))
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(multipart))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "logo upload: {body}");
    assert!(body["logo_url"].as_str().is_some_and(|u| !u.is_empty()));

    // The branding PATCH must not disturb the just-uploaded logo (the form
    // does logo-then-PATCH on a single Save).
    let (st, _) = read(
        router
            .clone()
            .oneshot(patch(
                &org,
                json!({ "public_status_enabled": true, "public_brand_color": "#3b82f6" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = read(
        router
            .oneshot(
                Request::get(format!("/api/v1/orgs/{org}/status-page"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        body["logo_url"].as_str().is_some_and(|u| !u.is_empty()),
        "logo survived the branding save: {body}"
    );
}
