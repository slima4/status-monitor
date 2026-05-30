//! Cross-tenant IDOR / BOLA regression (acceptance #9). Two SaaS orgs, one
//! target each, one shared `PostgresTargetStore`. A logged-in operator whose
//! active org is A must not be able to read org B's target — by config, by
//! results, by the operator HTML pages — even with B's exact UUID. Every
//! foreign lookup is a 404 (cloak: never confirm the row exists).
//!
//! Live-PG ignored: needs a Postgres pool. The results sink is in-memory, but
//! the per-target results endpoints gate on `target_store.get(org, id)` first,
//! so a foreign UUID is 404 regardless of the results backend.

mod common;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    build_saas_router_with_pg_targets, default_http_check, make_user, unique_slug, with_session,
};
use tower::ServiceExt;
use uptimepage::domain::{CheckSpec, ExpectedStatus, NewTarget};
use uptimepage::storage::{PostgresTargetStore, TargetStore, create_org_with_owner};
use url::Url;

fn a_target() -> NewTarget {
    let url = Url::parse("https://example.com/").unwrap();
    NewTarget {
        name: "secret-target".into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: Default::default(),
        group_name: None,
        owner_user_id: None,
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    }
}

async fn status_of(router: &Router, path: &str) -> StatusCode {
    router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn logged_in_operator_cannot_read_another_orgs_target() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };

    let user_a = make_user(&pool, "idor").await;
    let user_b = make_user(&pool, "idor").await;
    let org_a = create_org_with_owner(&pool, user_a, &unique_slug("idor-a"), "A", 3)
        .await
        .unwrap()
        .expect("org a")
        .id;
    let org_b = create_org_with_owner(&pool, user_b, &unique_slug("idor-b"), "B", 3)
        .await
        .unwrap()
        .expect("org b")
        .id;

    // One shared store; isolation comes from the org argument, exactly as in
    // production (`AppState` holds one store, the request supplies the org).
    let store = PostgresTargetStore::from_pool(pool.clone(), None);
    let t_a = store
        .create(org_a, a_target(), i64::MAX)
        .await
        .expect("create A's target");
    let t_b = store
        .create(org_b, a_target(), i64::MAX)
        .await
        .expect("create B's target");

    let router = build_saas_router_with_pg_targets(pool.clone()).await;
    let a = with_session(router.clone(), user_a, Some(org_a), Some("idor-test"));
    let b = with_session(router, user_b, Some(org_b), Some("idor-test"));

    // ── A may read its own target on every per-target surface ────────────
    for path in [
        format!("/api/v1/targets/{}", t_a.id),
        format!("/api/v1/targets/{}/results", t_a.id),
        format!("/api/v1/targets/{}/uptime", t_a.id),
        format!("/api/v1/targets/{}/incidents", t_a.id),
        format!("/targets/{}", t_a.id),
        format!("/targets/{}/edit", t_a.id),
    ] {
        assert_eq!(
            status_of(&a, &path).await,
            StatusCode::OK,
            "A must see its own target at {path}"
        );
    }

    // ── A must NOT read B's target on any surface — 404, not 403 ─────────
    for path in [
        format!("/api/v1/targets/{}", t_b.id),
        format!("/api/v1/targets/{}/results", t_b.id),
        format!("/api/v1/targets/{}/uptime", t_b.id),
        format!("/api/v1/targets/{}/incidents", t_b.id),
        format!("/targets/{}", t_b.id),
        format!("/targets/{}/edit", t_b.id),
    ] {
        assert_eq!(
            status_of(&a, &path).await,
            StatusCode::NOT_FOUND,
            "A must get 404 for B's target at {path}"
        );
    }

    // ── Symmetry: B cannot read A's target either ────────────────────────
    assert_eq!(
        status_of(&b, &format!("/api/v1/targets/{}", t_a.id)).await,
        StatusCode::NOT_FOUND,
        "B must get 404 for A's target"
    );
    assert_eq!(
        status_of(&b, &format!("/api/v1/targets/{}", t_b.id)).await,
        StatusCode::OK,
        "B still sees its own target"
    );

    // The list endpoint must not enumerate the other org's rows.
    let a_list = a
        .clone()
        .oneshot(Request::get("/api/v1/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(a_list.status(), StatusCode::OK);
    let body = axum::body::to_bytes(a_list.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains(&t_a.id.to_string()),
        "A's list includes A's target"
    );
    assert!(
        !body.contains(&t_b.id.to_string()),
        "A's list must not leak B's target id"
    );

    // Teardown: ON DELETE CASCADE wipes targets + memberships.
    let _ = sqlx::query("DELETE FROM organizations WHERE id = ANY($1)")
        .bind(vec![org_a.0, org_b.0])
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![user_a.0, user_b.0])
        .execute(&pool)
        .await;
}
