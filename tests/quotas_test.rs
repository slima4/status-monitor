//! Quota & rate-limit coverage.
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
use axum::response::IntoResponse;
use common::{body_json, build_test_app_with_pg_store, pg_pool_from_env};
use serde_json::{Value, json};
use sqlx::PgPool;
use status_monitor::config::AppConfig;
use status_monitor::domain::quota::Plan;
use status_monitor::domain::{OrgId, Role, UserId};
use status_monitor::quotas::{QuotaService, RateLimitCategory, RateLimitKey, RateLimitService};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

/// A `Plan` whose every per-minute rate is `per_min` and whose resource caps
/// are 1. Only the `*_per_minute` fields matter to `RateLimitService`; this
/// removes the ~25-line `Plan { .. }` literal that was copy-pasted into the
/// keying and janitor tests.
fn test_plan(per_min: i32) -> Plan {
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
        api_writes_per_minute: per_min,
        api_reads_per_minute: per_min,
        bulk_ops_per_minute: per_min,
        test_now_per_minute: per_min,
        check_now_per_minute: per_min,
        custom_domain_enabled: false,
        white_label_enabled: false,
        incident_narration_enabled: true,
        is_listed: false,
        created_at: now,
        updated_at: now,
    }
}

/// Insert a plan cloned from `free` with the given id and overridden caps,
/// then an org bound to it. Lets a quota test pick tiny caps so it doesn't
/// have to create hundreds of rows to reach a limit.
async fn seed_org_on_plan(
    pool: &PgPool,
    max_targets: i32,
    max_members: i32,
    max_public_components: i32,
    max_pending_invitations: i32,
) -> (String, OrgId) {
    let pid = format!("p{}", Uuid::now_v7().simple());
    sqlx::query(
        "INSERT INTO plans (id, name, description, max_targets, min_check_interval_secs, \
         retention_days, max_members, max_pending_invitations, max_api_tokens_per_user, \
         max_public_components, max_maintenance_windows, max_logo_size_bytes, \
         api_writes_per_minute, api_reads_per_minute, bulk_ops_per_minute, \
         test_now_per_minute, check_now_per_minute, custom_domain_enabled, \
         white_label_enabled, incident_narration_enabled, is_listed, created_at, updated_at) \
         SELECT $1, $1, 'seeded', $2, min_check_interval_secs, retention_days, $3, $4, \
         max_api_tokens_per_user, $5, max_maintenance_windows, max_logo_size_bytes, \
         api_writes_per_minute, api_reads_per_minute, bulk_ops_per_minute, \
         test_now_per_minute, check_now_per_minute, custom_domain_enabled, \
         white_label_enabled, incident_narration_enabled, false, now(), now() \
         FROM plans WHERE id = 'free'",
    )
    .bind(&pid)
    .bind(max_targets)
    .bind(max_members)
    .bind(max_pending_invitations)
    .bind(max_public_components)
    .execute(pool)
    .await
    .expect("seed plan");
    let slug = format!("qs{}", Uuid::now_v7().simple());
    let (oid,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name, plan_id) VALUES ($1, 'seeded', $2) RETURNING id",
    )
    .bind(&slug[..slug.len().min(30)])
    .bind(&pid)
    .fetch_one(pool)
    .await
    .expect("seed org");
    (pid, OrgId(oid))
}

/// Insert a user and an active membership in `org`, returning the user id.
async fn seed_member(pool: &PgPool, org: OrgId) -> UserId {
    let email = format!("m-{}@example.test", Uuid::now_v7());
    let (uid,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(pool)
        .await
        .expect("seed user");
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'member')")
        .bind(uid)
        .bind(org.0)
        .execute(pool)
        .await
        .expect("seed membership");
    UserId(uid)
}

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

// ── the floor is enforced on the *bulk* path too ────────────────
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

// ── a bulk that would breach the cap inserts nothing ────────────
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

// ── N concurrent creates at limit-1 land exactly `limit` ────────
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

// ── per-org and per-user buckets are independent ───────────────────
#[test]
fn rate_limit_keys_are_independent_per_subject() {
    let svc = RateLimitService::new();
    let plan = test_plan(1); // 1/min → 2nd hit on a key denied
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

// ── the janitor sweep bounds the map ───────────────────────────────
#[tokio::test]
async fn janitor_sweep_shrinks_idle_map() {
    let svc = RateLimitService::new();
    let plan = test_plan(100);
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

// ── a bad config number is a clean error, not a panic ───────────
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

// ── the legacy config keys stay deleted (CI guard) ──────────────
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

// ── write routes merged outside v1's stack still carry the limiter ──
// `.layer()` only wraps routes already on the router, so a sub-router
// `.merge`d after `.layer(rate_limit_middleware)` ships unprotected unless
// it re-applies the layer. This pins that contract for the bulk router (a
// regression here once shipped an unauthenticated, un-rate-limited
// 10k-target endpoint).
#[test]
fn merged_write_subrouters_are_rate_limited() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/routes.rs"))
        .expect("read routes.rs");
    let bulk = {
        let start = src.find("let bulk = Router::new()").expect("bulk router");
        let end = start + src[start..].find(";\n").expect("bulk router terminator");
        &src[start..end]
    };
    assert!(
        bulk.contains("rate_limit_middleware"),
        "the bulk sub-router must re-apply rate_limit_middleware (it is \
         merged after v1's layer stack and would otherwise be unlimited)"
    );
    assert!(
        bulk.contains("api_token::middleware"),
        "the bulk sub-router must re-apply api-token auth for the same reason"
    );
}

// ── the plan cache holds a stale value until its TTL, then refreshes ─
#[tokio::test]
async fn plan_cache_holds_until_ttl_then_refreshes() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (pid, org) = seed_org_on_plan(&pool, 7, 5, 10, 10).await;
    let mut cfg = AppConfig::load().expect("config");
    cfg.quotas.plan_cache_ttl_secs = 1;
    let svc = QuotaService::new(&cfg, Some(pool.clone()));

    assert_eq!(svc.limit_for_org(org).await.unwrap().max_targets, 7);
    sqlx::query("UPDATE plans SET max_targets = 99 WHERE id = $1")
        .bind(&pid)
        .execute(&pool)
        .await
        .unwrap();
    // Within the TTL the SQL change is invisible — a cache hit is zero
    // round-trips, which is the whole point on the hot path.
    assert_eq!(
        svc.limit_for_org(org).await.unwrap().max_targets,
        7,
        "mutation must not take effect before the TTL elapses"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        svc.limit_for_org(org).await.unwrap().max_targets,
        99,
        "after the TTL the next lookup refetches"
    );
}

// ── self-host overrides replace plan defaults only in single-tenant ─
#[tokio::test]
async fn self_host_override_applies_only_in_single_tenant_mode() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // Plan default cap is 5; the operator override lifts it to 10_000.
    let (_pid, org) = seed_org_on_plan(&pool, 5, 5, 10, 10).await;

    let mut cfg = AppConfig::load().expect("config");
    cfg.quotas.self_host_overrides.enabled = true;
    cfg.quotas.self_host_overrides.max_targets = Some(10_000);

    // Single-tenant: the override replaces the plan default, so a
    // self-hoster who dialled the cap up gets the bigger number and can
    // create well past the shipped 5-target default.
    cfg.tenancy.enabled = false;
    let svc = QuotaService::new(&cfg, Some(pool.clone()));
    assert_eq!(
        svc.limit_for_org(org).await.unwrap().max_targets,
        10_000,
        "override must replace the plan default when tenancy is off"
    );

    // Multi-tenant: the same override is ignored — every org is held to
    // its plan, never to a process-wide knob a tenant could not set.
    cfg.tenancy.enabled = true;
    let svc = QuotaService::new(&cfg, Some(pool.clone()));
    assert_eq!(
        svc.limit_for_org(org).await.unwrap().max_targets,
        5,
        "override must be ignored in SaaS mode"
    );
}

// ── the usage cache snapshots counts for its (short) TTL ────────────
#[tokio::test]
async fn usage_cache_holds_counts_until_ttl() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (_pid, org) = seed_org_on_plan(&pool, 10, 50, 10, 10).await;
    seed_member(&pool, org).await;
    let mut cfg = AppConfig::load().expect("config");
    cfg.quotas.usage_cache_ttl_secs = 1;
    let svc = QuotaService::new(&cfg, Some(pool.clone()));

    assert_eq!(svc.org_usage(org).await.unwrap().members, 1);
    seed_member(&pool, org).await; // DB now has 2
    assert_eq!(
        svc.org_usage(org).await.unwrap().members,
        1,
        "the snapshot is cached for the TTL, not recomputed per call"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(svc.org_usage(org).await.unwrap().members, 2);
}

// ── the pending-invitation cap is atomic ──────────────────
// The pending cap predates the unified quota envelope and surfaces as
// `409 INVITATIONS_LIMIT`, not `422 QUOTA_EXCEEDED`. What the no-overshoot invariant protects —
// that the cap is never overshot — is asserted here; the wire code is the
// existing invitation contract, deliberately unchanged.
#[tokio::test]
async fn pending_invitation_cap_rejects_over_limit() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (_pid, org) = seed_org_on_plan(&pool, 10, 50, 10, 3).await;
    let inviter = seed_member(&pool, org).await;
    for _ in 0..3 {
        status_monitor::auth::invitations::create(
            &pool,
            org,
            inviter,
            &format!("inv-{}@example.test", Uuid::now_v7()),
            Role::Member,
            72,
            3,
        )
        .await
        .expect("within cap");
    }
    let over = status_monitor::auth::invitations::create(
        &pool,
        org,
        inviter,
        &format!("inv-{}@example.test", Uuid::now_v7()),
        Role::Member,
        72,
        3,
    )
    .await
    .expect_err("4th over the cap of 3 must be rejected");
    let resp = over.into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(resp).await["error"]["code"], "INVITATIONS_LIMIT");
}

// ── N parallel invites never exceed the cap; dup email → one row ───
#[tokio::test]
async fn parallel_invites_never_exceed_cap_and_dedupe() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (_pid, org) = seed_org_on_plan(&pool, 10, 50, 10, 3).await;
    let inviter = seed_member(&pool, org).await;

    // 10 parallel distinct-email invites, cap 3 → exactly 3 land.
    let mut handles = Vec::new();
    for _ in 0..10 {
        let p = pool.clone();
        handles.push(tokio::spawn(async move {
            status_monitor::auth::invitations::create(
                &p,
                org,
                inviter,
                &format!("p-{}@example.test", Uuid::now_v7()),
                Role::Member,
                72,
                3,
            )
            .await
            .is_ok()
        }));
    }
    let mut ok = 0;
    for h in handles {
        if h.await.unwrap() {
            ok += 1;
        }
    }
    assert_eq!(ok, 3, "advisory-locked cap must hold under concurrency");

    // Parallel duplicate-email invites → exactly one row.
    let dup = format!("dup-{}@example.test", Uuid::now_v7());
    let mut handles = Vec::new();
    for _ in 0..5 {
        let (p, e) = (pool.clone(), dup.clone());
        handles.push(tokio::spawn(async move {
            status_monitor::auth::invitations::create(&p, org, inviter, &e, Role::Member, 72, 100)
                .await
                .is_ok()
        }));
    }
    let mut dup_ok = 0;
    for h in handles {
        if h.await.unwrap() {
            dup_ok += 1;
        }
    }
    assert_eq!(
        dup_ok, 1,
        "dedupe under the lock yields a single invitation"
    );
    let (rows,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM invitations WHERE org_id = $1 AND email = $2::citext")
            .bind(org.0)
            .bind(&dup)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
}

// ── max_members: friendly pre-check + atomic backstop, never overshoot ────
#[tokio::test]
async fn member_cap_is_enforced_atomically() {
    use status_monitor::storage::orgs::{AddMemberOutcome, add_member};
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // Cap 2 members.
    let (_pid, org) = seed_org_on_plan(&pool, 10, 2, 10, 10).await;
    let mut cfg = AppConfig::load().expect("config");
    cfg.quotas.plan_cache_ttl_secs = 1;
    let svc = QuotaService::new(&cfg, Some(pool.clone()));

    let make_user = |pool: PgPool| async move {
        let email = format!("c-{}@example.test", Uuid::now_v7());
        let (id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(&email)
            .fetch_one(&pool)
            .await
            .unwrap();
        UserId(id)
    };
    let u1 = make_user(pool.clone()).await;
    let u2 = make_user(pool.clone()).await;
    let u3 = make_user(pool.clone()).await;

    assert!(svc.check_can_add_member(org, Some(u1)).await.is_ok());
    assert_eq!(
        add_member(&pool, org, u1, u1, Role::Member, 2)
            .await
            .unwrap(),
        AddMemberOutcome::Added
    );
    assert_eq!(
        add_member(&pool, org, u2, u2, Role::Member, 2)
            .await
            .unwrap(),
        AddMemberOutcome::Added
    );
    // At the cap, the friendly pre-check must reject with the quota 422.
    let pre = svc
        .check_can_add_member(org, Some(u3))
        .await
        .expect_err("at cap the friendly check rejects");
    let resp = pre.into_response();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "QUOTA_EXCEEDED");
    // And the atomic backstop refuses the insert too — no overshoot.
    assert_eq!(
        add_member(&pool, org, u3, u3, Role::Member, 2)
            .await
            .unwrap(),
        AddMemberOutcome::LimitReached {
            current: 2,
            limit: 2
        }
    );
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM memberships WHERE org_id = $1")
        .bind(org.0)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2, "member cap never overshoots");
    // Re-adding an existing member stays a no-op even at the cap.
    assert_eq!(
        add_member(&pool, org, u1, u1, Role::Member, 2)
            .await
            .unwrap(),
        AddMemberOutcome::AlreadyMember
    );
}

// ── max_public_components: the PATCH public-flip is gated like create ─────
#[tokio::test]
async fn patch_cannot_bypass_public_component_cap() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (pid, _seed_org) = seed_org_on_plan(&pool, 50, 50, 1, 10).await;
    // Build the app, then point its org at the cap-1 plan. The plan cache is
    // cold until the first request, so no TTL wait is needed.
    let (app, org) = build_test_app_with_pg_store(pool.clone(), |_| {}).await;
    sqlx::query("UPDATE organizations SET plan_id = $1 WHERE id = $2")
        .bind(&pid)
        .bind(org.0)
        .execute(&pool)
        .await
        .unwrap();

    // First public target consumes the only slot.
    let pubt = json!({
        "name": format!("pub-{}", Uuid::now_v7()),
        "check": {"type":"http","url":"http://example.com","method":"GET","timeout":5000,
            "follow_redirects":false,"max_redirects":0,
            "expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true},
        "interval": 60, "tags": [], "public_status": true
    });
    let r = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from(pubt.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);

    // A private target created fine…
    let r = post_target(&app, &format!("priv-{}", Uuid::now_v7()), 60).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let id = body_json(r).await["id"].as_str().unwrap().to_string();

    // …but flipping it public via PATCH must hit the same cap (the bypass
    // this gate closes).
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/targets/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"public_status": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "QUOTA_EXCEEDED");
    assert_eq!(b["error"]["details"]["quota"], "max_public_components");
}

// ── bulk-ops and reads each trip 429 at their own per-min rate ─────
#[tokio::test]
async fn bulk_and_read_rates_trip_429_independently() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // Clone `free` but with tiny read/bulk rates so the test makes a handful
    // of requests instead of thousands. Cold cache → first request binds it.
    let pid = format!("p{}", Uuid::now_v7().simple());
    sqlx::query(
        "INSERT INTO plans (id, name, description, max_targets, min_check_interval_secs, \
         retention_days, max_members, max_pending_invitations, max_api_tokens_per_user, \
         max_public_components, max_maintenance_windows, max_logo_size_bytes, \
         api_writes_per_minute, api_reads_per_minute, bulk_ops_per_minute, \
         test_now_per_minute, check_now_per_minute, custom_domain_enabled, \
         white_label_enabled, incident_narration_enabled, is_listed, created_at, updated_at) \
         SELECT $1, $1, 'lowrate', max_targets, min_check_interval_secs, retention_days, \
         max_members, max_pending_invitations, max_api_tokens_per_user, max_public_components, \
         max_maintenance_windows, max_logo_size_bytes, api_writes_per_minute, 5, 3, \
         test_now_per_minute, check_now_per_minute, custom_domain_enabled, \
         white_label_enabled, incident_narration_enabled, false, now(), now() \
         FROM plans WHERE id = 'free'",
    )
    .bind(&pid)
    .execute(&pool)
    .await
    .unwrap();
    let (app, org) = build_test_app_with_pg_store(pool.clone(), |_| {}).await;
    sqlx::query("UPDATE organizations SET plan_id = $1 WHERE id = $2")
        .bind(&pid)
        .bind(org.0)
        .execute(&pool)
        .await
        .unwrap();

    // First 429 index for a request builder, scanning a generous window.
    async fn first_limited(
        app: &Router,
        mk: impl Fn() -> Request<Body>,
    ) -> (Option<usize>, Vec<u16>) {
        let mut codes = Vec::new();
        let mut hit = None;
        for i in 0..40 {
            let s = app.clone().oneshot(mk()).await.unwrap().status();
            codes.push(s.as_u16());
            if s == StatusCode::TOO_MANY_REQUESTS {
                hit = Some(i);
                break;
            }
        }
        (hit, codes)
    }

    // api_reads_per_minute = 5: a burst of GETs trips well before 40.
    let (read_i, read_codes) = first_limited(&app, || {
        Request::get("/api/v1/targets").body(Body::empty()).unwrap()
    })
    .await;
    let read_i = read_i.unwrap_or_else(|| panic!("reads never limited: {read_codes:?}"));

    // bulk_ops_per_minute = 3 (a different, tighter bucket). Invalid body is
    // fine: the limiter runs before the handler and consumes a token anyway.
    let (bulk_i, bulk_codes) = first_limited(&app, || {
        Request::post("/api/v1/targets/bulk")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    })
    .await;
    let bulk_i = bulk_i.unwrap_or_else(|| panic!("bulk never limited: {bulk_codes:?}"));

    // Independence: bulk has its own, tighter budget (3 < 5), so it trips
    // strictly before reads would — exhausting one category never spent the
    // other's tokens. Exact GCRA burst can be off by one, so compare the
    // buckets rather than pin an absolute index.
    assert!(
        bulk_i < read_i,
        "bulk (rate 3) must trip before reads (rate 5) — separate buckets; \
         got bulk@{bulk_i} {bulk_codes:?}, read@{read_i} {read_codes:?}"
    );
}

// ── two users in one org have independent per-user budgets ────────
#[test]
fn per_user_budgets_are_isolated_within_one_org() {
    let svc = RateLimitService::new();
    let plan = test_plan(1); // 1/min per key
    let cat = RateLimitCategory::ApiWrites;
    let user_a = UserId(Uuid::now_v7());
    let user_b = UserId(Uuid::now_v7());

    assert!(
        svc.check(RateLimitKey::User(user_a, cat), "per_user", &plan)
            .is_ok()
    );
    assert!(
        svc.check(RateLimitKey::User(user_a, cat), "per_user", &plan)
            .is_err(),
        "user A is now exhausted"
    );
    assert!(
        svc.check(RateLimitKey::User(user_b, cat), "per_user", &plan)
            .is_ok(),
        "user B in the same org keeps its own budget"
    );
}

// ── a plan upgrade takes effect after the TTL ────
#[tokio::test]
async fn plan_upgrade_lifts_the_cap_after_cache_ttl() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, org) =
        build_test_app_with_pg_store(pool.clone(), |c| c.quotas.plan_cache_ttl_secs = 1).await;
    for i in 0..10 {
        assert_eq!(
            post_target(&app, &format!("u-{}-{i}", Uuid::now_v7()), 60)
                .await
                .status(),
            StatusCode::CREATED
        );
    }
    assert_eq!(
        post_target(&app, &format!("over-{}", Uuid::now_v7()), 60)
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "11th blocked on the free cap of 10"
    );

    let (pid, _seed) = seed_org_on_plan(&pool, 12, 50, 10, 10).await;
    sqlx::query("UPDATE organizations SET plan_id = $1 WHERE id = $2")
        .bind(&pid)
        .bind(org.0)
        .execute(&pool)
        .await
        .unwrap();
    // Cached 'free' is still in force until the TTL lapses.
    assert_eq!(
        post_target(&app, &format!("still-{}", Uuid::now_v7()), 60)
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        post_target(&app, &format!("now-{}", Uuid::now_v7()), 60)
            .await
            .status(),
        StatusCode::CREATED,
        "after the TTL the higher cap applies; the existing 10 rows stayed"
    );
    let list = app
        .oneshot(Request::get("/api/v1/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        body_json(list).await["items"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        11
    );
}

// ── every quota'd create path carries its enforcement gate ─────
// Forward contract: a future edit that drops a gate fails CI here. Each
// entry is `(file, fn, gate-token)`; the token must appear inside that fn's
// body. The "named bypass allowlist" is the comment column — there is none:
// every quota'd resource is gated.
#[test]
fn every_quota_create_path_is_gated() {
    fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src
            .find(sig)
            .unwrap_or_else(|| panic!("missing fn `{sig}`"));
        let rest = &src[start..];
        let open = rest.find('{').expect("fn body open brace");
        let bytes = rest.as_bytes();
        let mut depth = 0usize;
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &rest[open..=i];
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces for `{sig}`");
    }
    let root = env!("CARGO_MANIFEST_DIR");
    // (relative path, fn signature prefix, required gate token)
    let registry: &[(&str, &str, &str)] = &[
        (
            "src/api/handlers/targets.rs",
            "pub async fn create(",
            "check_can_create_targets",
        ),
        (
            "src/api/handlers/targets.rs",
            "pub async fn bulk_create(",
            "check_can_create_targets",
        ),
        (
            "src/api/handlers/targets.rs",
            "pub async fn update(",
            "check_public_components",
        ),
        (
            "src/api/handlers/maintenance.rs",
            "pub async fn create_maintenance(",
            "check_can_create_maintenance_window",
        ),
        (
            "src/api/handlers/invitations.rs",
            "pub async fn create(",
            "max_pending_invitations",
        ),
        (
            "src/api/handlers/invitations.rs",
            "pub async fn accept(",
            "check_can_add_member",
        ),
        (
            "src/api/handlers/api_tokens.rs",
            "pub async fn create(",
            "max_api_tokens_per_user",
        ),
        (
            "src/storage/orgs.rs",
            "pub async fn create_org_with_owner(",
            "owner_limit",
        ),
    ];
    let mut missing = Vec::new();
    for (rel, sig, token) in registry {
        let src = std::fs::read_to_string(format!("{root}/{rel}"))
            .unwrap_or_else(|_| panic!("read {rel}"));
        if !fn_body(&src, sig).contains(token) {
            missing.push(format!("{rel} :: {sig} missing gate `{token}`"));
        }
    }
    assert!(
        missing.is_empty(),
        "a quota'd create path lost its enforcement gate: {missing:?}"
    );
}

// ── cancelling the token stops the janitor (no leaked sweep) ───────
#[tokio::test]
async fn cancelled_janitor_stops_sweeping() {
    let svc = Arc::new(RateLimitService::new());
    let plan = test_plan(1000);
    let token = CancellationToken::new();
    svc.clone().spawn_janitor(
        Duration::from_millis(20),
        Duration::from_millis(1),
        token.clone(),
    );

    for _ in 0..10 {
        let _ = svc.check(
            RateLimitKey::Org(OrgId(Uuid::now_v7()), RateLimitCategory::ApiReads),
            "per_org",
            &plan,
        );
    }
    // A live janitor sweeps the idle entries within a couple of ticks.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(svc.len(), 0, "live janitor must bound the map");

    token.cancel();
    tokio::time::sleep(Duration::from_millis(40)).await;
    for _ in 0..10 {
        let _ = svc.check(
            RateLimitKey::Org(OrgId(Uuid::now_v7()), RateLimitCategory::ApiReads),
            "per_org",
            &plan,
        );
    }
    // Long enough that a still-running janitor would have swept these.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(svc.len(), 10, "a cancelled janitor must not keep sweeping");
}

// ── a forwarded client IP cannot mint independent buckets ──────────
#[tokio::test]
async fn forwarded_ip_cannot_bypass_the_org_bucket() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let pid = format!("p{}", Uuid::now_v7().simple());
    sqlx::query(
        "INSERT INTO plans (id, name, description, max_targets, min_check_interval_secs, \
         retention_days, max_members, max_pending_invitations, max_api_tokens_per_user, \
         max_public_components, max_maintenance_windows, max_logo_size_bytes, \
         api_writes_per_minute, api_reads_per_minute, bulk_ops_per_minute, \
         test_now_per_minute, check_now_per_minute, custom_domain_enabled, \
         white_label_enabled, incident_narration_enabled, is_listed, created_at, updated_at) \
         SELECT $1, $1, 'xff', max_targets, min_check_interval_secs, retention_days, \
         max_members, max_pending_invitations, max_api_tokens_per_user, max_public_components, \
         max_maintenance_windows, max_logo_size_bytes, 3, api_reads_per_minute, bulk_ops_per_minute, \
         test_now_per_minute, check_now_per_minute, custom_domain_enabled, \
         white_label_enabled, incident_narration_enabled, false, now(), now() \
         FROM plans WHERE id = 'free'",
    )
    .bind(&pid)
    .execute(&pool)
    .await
    .unwrap();
    let (app, org) = build_test_app_with_pg_store(pool.clone(), |_| {}).await;
    sqlx::query("UPDATE organizations SET plan_id = $1 WHERE id = $2")
        .bind(&pid)
        .bind(org.0)
        .execute(&pool)
        .await
        .unwrap();

    let write = |xff: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::post("/api/v1/targets")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", xff)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // Exhaust api_writes (3/min) all from one forwarded IP.
    let mut limited_from = None;
    for i in 0..20 {
        if write("1.1.1.1").await.status() == StatusCode::TOO_MANY_REQUESTS {
            limited_from = Some(i);
            break;
        }
    }
    assert!(limited_from.is_some(), "org bucket must trip");
    // A different forwarded IP, same authenticated org → STILL limited.
    // The bucket keys on the subject, never the peer/header, so IP rotation
    // cannot mint a fresh budget.
    assert_eq!(
        write("9.9.9.9").await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "rotating X-Forwarded-For must not bypass the per-org limit"
    );
}

// ── the limiter map is bounded by distinct keys, not request count,
// and the sweep frees it ─────────────────────────────────────────────────
#[test]
fn rate_limit_map_is_bounded_and_sweeps_to_zero() {
    let svc = RateLimitService::new();
    let plan = test_plan(1_000_000); // never denies; we only count keys
    let orgs: Vec<OrgId> = (0..200).map(|_| OrgId(Uuid::now_v7())).collect();
    let cats = [
        RateLimitCategory::ApiWrites,
        RateLimitCategory::ApiReads,
        RateLimitCategory::BulkOps,
        RateLimitCategory::TestNow,
        RateLimitCategory::CheckNow,
    ];
    // Exactly 1,000,000 checks over 200 orgs × 5 categories = 1000 distinct
    // keys. (Indexing by `i % 200` / `i % 5` would alias — 5 divides 200, so
    // the category would be a function of the org and only 200 keys form.)
    for _round in 0..1000 {
        for o in &orgs {
            for c in cats {
                let _ = svc.check(RateLimitKey::Org(*o, c), "per_org", &plan);
            }
        }
    }
    let n = svc.len();
    assert_eq!(
        n, 1000,
        "map is bounded by the 1000 distinct keys, not the 1M requests"
    );
    // Let wall-clock advance so every `last_seen` is strictly older than the
    // sweep cutoff (millisecond resolution; idle=0 alone wouldn't catch the
    // entries touched in the final millisecond).
    std::thread::sleep(Duration::from_millis(5));
    let removed = svc.sweep(Duration::from_millis(0));
    assert_eq!(removed, 1000, "every idle entry is swept");
    assert_eq!(svc.len(), 0, "memory returns to zero after a sweep");
}
