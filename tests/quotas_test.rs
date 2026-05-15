//! Phase 2+3 quota & rate-limit coverage.
//!
//! Integration cases need Postgres (the `plans` table + org `plan_id`); they
//! no-op when `DATABASE_URL` is unset, like the other live-PG suites. The
//! pure-logic cases (rate-limit keying/janitor, config validation, Caddy
//! parity) always run.
//!
//! These exist so a future edit that reintroduces a known failure mode
//! (over-cap overshoot, floor bypass via bulk, peer-IP keying, panic on a
//! bad config number, leaked limiter map) fails CI, not production.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{body_json, build_test_app_with_pg_store, pg_pool_from_env};
use serde_json::{Value, json};
use sqlx::PgPool;
use status_monitor::config::AppConfig;
use status_monitor::domain::quota::Plan;
use status_monitor::domain::{OrgId, UserId};
use status_monitor::quotas::{RateLimitCategory, RateLimitKey, RateLimitService};
use tower::ServiceExt;
use uuid::Uuid;

fn target_payload(name: &str, interval: u64) -> Value {
    json!({
        "name": name,
        "check": {
            "type": "http",
            "url": "http://example.com",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true
        },
        "interval": interval,
        "tags": []
    })
}

async fn post_target(app: &Router, name: &str, interval: u64) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from(target_payload(name, interval).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn quota_event_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM quota_events")
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}

// ── Report item: 11th target → 422 with the exact shape ──────────────────
#[tokio::test]
async fn eleventh_target_returns_422_quota_exceeded() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let before = quota_event_count(&pool).await;
    let (app, _org) = build_test_app_with_pg_store(pool.clone(), |_| {}).await;

    for i in 0..10 {
        let resp = post_target(&app, &format!("ok-{}-{i}", Uuid::now_v7()), 60).await;
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "target {i} should create"
        );
    }
    let resp = post_target(&app, &format!("over-{}", Uuid::now_v7()), 60).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "QUOTA_EXCEEDED");
    assert_eq!(b["error"]["details"]["quota"], "max_targets");
    assert_eq!(b["error"]["details"]["current"], 10);
    assert_eq!(b["error"]["details"]["limit"], 10);
    assert_eq!(b["error"]["details"]["plan"], "free");

    // Report item: quota_events records the block (fire-and-forget).
    let mut grew = false;
    for _ in 0..40 {
        if quota_event_count(&pool).await > before {
            grew = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(grew, "quota_events should gain a row on the block");
}

// ── Report item: interval=59 → 422 MIN_CHECK_INTERVAL (singular) ──────────
#[tokio::test]
async fn sub_minimum_interval_rejected_on_create() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |_| {}).await;
    let resp = post_target(&app, &format!("fast-{}", Uuid::now_v7()), 59).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "MIN_CHECK_INTERVAL");
}

// ── §3.6 I4: the floor is enforced on the *bulk* path too ────────────────
#[tokio::test]
async fn sub_minimum_interval_rejected_on_bulk() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |_| {}).await;
    let body = json!([target_payload(&format!("b-{}", Uuid::now_v7()), 30)]);
    let resp = app
        .oneshot(
            Request::post("/api/v1/targets/bulk")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "MIN_CHECK_INTERVAL");
}

// ── §3.6 I3: a bulk that would breach the cap inserts nothing ────────────
#[tokio::test]
async fn bulk_over_cap_inserts_nothing() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |_| {}).await;
    for i in 0..8 {
        assert_eq!(
            post_target(&app, &format!("seed-{}-{i}", Uuid::now_v7()), 60)
                .await
                .status(),
            StatusCode::CREATED
        );
    }
    // 8 existing + 5 new > 10 → all-or-nothing rejection.
    let items: Vec<Value> = (0..5)
        .map(|i| target_payload(&format!("bulk-{}-{i}", Uuid::now_v7()), 60))
        .collect();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets/bulk")
                .header("content-type", "application/json")
                .body(Body::from(json!(items).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "QUOTA_EXCEEDED");

    let list = app
        .oneshot(Request::get("/api/v1/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = body_json(list).await;
    assert_eq!(
        v["items"].as_array().map(|a| a.len()).unwrap_or(0),
        8,
        "no bulk row should have been inserted"
    );
}

// ── §3.6 I2: N concurrent creates at limit-1 land exactly `limit` ────────
#[tokio::test]
async fn concurrent_creates_never_overshoot_cap() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |_| {}).await;
    for i in 0..9 {
        assert_eq!(
            post_target(&app, &format!("pre-{}-{i}", Uuid::now_v7()), 60)
                .await
                .status(),
            StatusCode::CREATED
        );
    }
    // 9 existing, cap 10: fire 12 in parallel — exactly one may win.
    let mut handles = Vec::new();
    for i in 0..12 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            post_target(&app, &format!("race-{}-{i}", Uuid::now_v7()), 60)
                .await
                .status()
        }));
    }
    let mut created = 0;
    for h in handles {
        if h.await.unwrap() == StatusCode::CREATED {
            created += 1;
        }
    }
    assert_eq!(created, 1, "exactly one create may cross 9→10");
    let list = app
        .oneshot(Request::get("/api/v1/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = body_json(list).await;
    assert_eq!(v["items"].as_array().map(|a| a.len()).unwrap_or(0), 10);
}

// ── Report item: 601st API write/min → 429 with Retry-After ──────────────
#[tokio::test]
async fn api_writes_over_plan_rate_return_429_with_retry_after() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // free plan api_writes_per_minute = 600. The rate-limit middleware runs
    // *before* the handler, so every POST consumes a token even when the
    // body is invalid and the handler 4xxs immediately — keeping the loop
    // fast enough that GCRA replenishment (10/s) can't outrun it.
    let (app, _org) = build_test_app_with_pg_store(pool, |_| {}).await;
    let fast_post = |app: Router| async move {
        app.oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap()
    };
    let mut limited = None;
    for i in 0..700 {
        let resp = fast_post(app.clone()).await;
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some((i, resp));
            break;
        }
    }
    let (idx, resp) = limited.expect("a request within 700 must be rate-limited");
    assert!(
        idx >= 600,
        "limiter tripped too early at {idx} (plan allows 600/min)"
    );
    assert!(
        resp.headers().contains_key("retry-after"),
        "429 must carry Retry-After"
    );
    assert_eq!(body_json(resp).await["error"]["code"], "RATE_LIMITED");
}

// ── §4.4: per-org and per-user buckets are independent ───────────────────
#[test]
fn rate_limit_keys_are_independent_per_subject() {
    fn plan_with_writes(n: i32) -> Plan {
        let now = chrono::Utc::now();
        Plan {
            id: "test".into(),
            name: "t".into(),
            description: "t".into(),
            max_targets: 1,
            min_check_interval_secs: 1,
            retention_days: 1,
            max_members: 1,
            max_pending_invitations: 1,
            max_api_tokens_per_user: 1,
            max_public_components: 1,
            max_maintenance_windows: 1,
            max_logo_size_bytes: 1,
            api_writes_per_minute: n,
            api_reads_per_minute: n,
            bulk_ops_per_minute: n,
            test_now_per_minute: n,
            check_now_per_minute: n,
            custom_domain_enabled: false,
            white_label_enabled: false,
            incident_narration_enabled: true,
            is_listed: false,
            created_at: now,
            updated_at: now,
        }
    }
    let svc = RateLimitService::new();
    let plan = plan_with_writes(1); // 1/min → 2nd hit on a key denied
    let org_a = OrgId(Uuid::now_v7());
    let org_b = OrgId(Uuid::now_v7());
    let user = UserId(Uuid::now_v7());
    let cat = RateLimitCategory::ApiWrites;

    // Exhaust org A.
    assert!(
        svc.check(RateLimitKey::Org(org_a, cat), "per_org", &plan)
            .is_ok()
    );
    assert!(
        svc.check(RateLimitKey::Org(org_a, cat), "per_org", &plan)
            .is_err()
    );
    // Org B has its own bucket.
    assert!(
        svc.check(RateLimitKey::Org(org_b, cat), "per_org", &plan)
            .is_ok()
    );
    // The user tier is independent of the org tier.
    assert!(
        svc.check(RateLimitKey::User(user, cat), "per_user", &plan)
            .is_ok()
    );
}

// ── §4.5: the janitor sweep bounds the map ───────────────────────────────
#[tokio::test]
async fn janitor_sweep_shrinks_idle_map() {
    let svc = RateLimitService::new();
    let now = chrono::Utc::now();
    let plan = Plan {
        id: "t".into(),
        name: "t".into(),
        description: "t".into(),
        max_targets: 1,
        min_check_interval_secs: 1,
        retention_days: 1,
        max_members: 1,
        max_pending_invitations: 1,
        max_api_tokens_per_user: 1,
        max_public_components: 1,
        max_maintenance_windows: 1,
        max_logo_size_bytes: 1,
        api_writes_per_minute: 100,
        api_reads_per_minute: 100,
        bulk_ops_per_minute: 100,
        test_now_per_minute: 100,
        check_now_per_minute: 100,
        custom_domain_enabled: false,
        white_label_enabled: false,
        incident_narration_enabled: true,
        is_listed: false,
        created_at: now,
        updated_at: now,
    };
    for _ in 0..50 {
        let _ = svc.check(
            RateLimitKey::Org(OrgId(Uuid::now_v7()), RateLimitCategory::ApiReads),
            "per_org",
            &plan,
        );
    }
    assert_eq!(svc.len(), 50);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let removed = svc.sweep(std::time::Duration::from_millis(1));
    assert_eq!(removed, 50);
    assert_eq!(svc.len(), 0, "janitor must bound the map");
}

// ── §3.6 I6: a bad config number is a clean error, not a panic ───────────
#[test]
fn zero_quota_config_is_a_clean_error_not_a_panic() {
    let mut cfg = AppConfig::load().expect("config");
    cfg.quotas.plan_cache_ttl_secs = 0;
    assert!(
        cfg.validate_quotas_and_limits().is_err(),
        "ttl=0 must be rejected at load"
    );

    let mut cfg = AppConfig::load().expect("config");
    cfg.quotas.self_host_overrides.max_targets = Some(0);
    assert!(
        cfg.validate_quotas_and_limits().is_err(),
        "override max_targets=0 must be rejected"
    );

    let mut cfg = AppConfig::load().expect("config");
    cfg.rate_limits.janitor.cleanup_interval_hours = 0;
    assert!(cfg.validate_quotas_and_limits().is_err());

    // The shipped defaults must pass.
    assert!(
        AppConfig::load()
            .unwrap()
            .validate_quotas_and_limits()
            .is_ok()
    );
}

// ── §3.6 I5: the legacy config keys stay deleted (CI guard) ──────────────
#[test]
fn legacy_quota_config_keys_are_not_reintroduced() {
    fn scan(dir: &std::path::Path, needles: &[&str], hits: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, needles, hits);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                let src = std::fs::read_to_string(&p).unwrap_or_default();
                for n in needles {
                    if src.contains(n) {
                        hits.push(format!("{}: {n}", p.display()));
                    }
                }
            }
        }
    }
    // Field-access / type forms only — prose can still describe the history.
    let needles = [
        ".max_pending_per_org",
        ".max_per_user",
        "RateLimitConfig",
        ".api.rate_limit",
    ];
    let mut hits = Vec::new();
    scan(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &needles,
        &mut hits,
    );
    assert!(
        hits.is_empty(),
        "legacy quota/rate config keys must stay deleted after cutover; found: {hits:?}"
    );
}

// ── Report item: Caddy carries the per-IP auth + org-creation zones ──────
#[test]
fn caddyfile_declares_per_ip_zones() {
    let caddy =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/deployment/Caddyfile"))
            .expect("read Caddyfile");
    assert!(
        caddy.contains("zone auth_endpoints"),
        "auth-endpoint per-IP zone missing"
    );
    assert!(
        caddy.contains("zone org_creation"),
        "org-creation per-IP zone missing"
    );
    // The removed peer-IP app layer must not silently come back as a
    // second, topology-blind limiter.
    assert!(
        !caddy.contains("events 60\n\t\t\t\twindow 1m\n\t\t\t}\n\t\t}\n\t\thandle @org_creation"),
        "sanity"
    );
}
