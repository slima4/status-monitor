//! Plan ceilings applied to the scheduler set, the agent pull, and its etag.
//! Live PG only; no-ops without `DATABASE_URL`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    body_json, build_test_app_with_pg_store, default_http_check, make_user, pg_pool_from_env,
    unique_slug,
};
use sqlx::PgPool;
use tower::ServiceExt;
use uptimepage::config::AppConfig;
use uptimepage::domain::{CheckSpec, ExpectedStatus, HeartbeatCheck, OrgId, Target, UserId};
use uptimepage::quotas::{PlanGoverned, QuotaService, governed_interval};
use uptimepage::storage::admin::{AdminRepo, EnabledTargetSource, EnabledTargetStream};
use uuid::Uuid;

async fn org_on_plan(pool: &PgPool, plan: &str, interval_secs: i32) -> (OrgId, Uuid, UserId) {
    let user = make_user(pool, "govern").await;
    let (account,): (Uuid,) = sqlx::query_as(
        "INSERT INTO accounts (owner_user_id, plan_id) VALUES ($1, $2) \
         ON CONFLICT (owner_user_id) WHERE owner_user_id IS NOT NULL \
         DO UPDATE SET plan_id = excluded.plan_id RETURNING id",
    )
    .bind(user.0)
    .bind(plan)
    .fetch_one(pool)
    .await
    .expect("account");
    let (org,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name, account_id) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(unique_slug("gov"))
    .bind("Governed")
    .bind(account)
    .fetch_one(pool)
    .await
    .expect("org");
    let (target,): (Uuid,) = sqlx::query_as(
        "INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled) \
         VALUES ($1, 'probe', $2, $3, true) RETURNING id",
    )
    .bind(org)
    // decode_targets_skipping silently drops a row whose spec will not parse.
    .bind(
        serde_json::to_value(CheckSpec::Http(default_http_check(
            "https://example.com".parse().expect("url"),
            ExpectedStatus::Exact(200),
        )))
        .expect("check spec"),
    )
    .bind(interval_secs)
    .fetch_one(pool)
    .await
    .expect("target");
    (OrgId(org), target, user)
}

async fn cleanup(pool: &PgPool, org: OrgId, user: UserId) {
    let account: Option<(Uuid,)> =
        sqlx::query_as("SELECT account_id FROM organizations WHERE id = $1")
            .bind(org.0)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org.0)
        .execute(pool)
        .await;
    if let Some((account,)) = account {
        let _ = sqlx::query("DELETE FROM plan_overrides WHERE account_id = $1")
            .bind(account)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM accounts WHERE id = $1")
            .bind(account)
            .execute(pool)
            .await;
    }
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(pool)
        .await;
}

fn sms_config(to: &str, auth_token: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "sms",
        "provider": "twilio",
        "to": to,
        "account_sid": "AC00000000000000000000000000000000",
        "auth_token": auth_token,
        "from": "+15559876543"
    })
}

fn sms_create_request(name: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/notification-channels")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "name": name,
                "config": sms_config("+15551234567", "tok"),
            })
            .to_string(),
        ))
        .unwrap()
}

async fn plan_digest_for(pool: &PgPool, region: &str) -> String {
    let cfg = AppConfig::load().expect("config");
    let quotas = QuotaService::new(&cfg, Some(pool.clone()));
    let repo = AdminRepo::new(pool.clone(), None, "plan_governance_test");
    let mut plans = std::collections::HashMap::new();
    for org in repo.region_org_ids(region).await.expect("org ids") {
        plans.insert(org, quotas.limit_for_org(org).await.ok());
    }
    uptimepage::quotas::effective::plan_digest(&plans)
}

fn governed_source(pool: &PgPool, cfg: &AppConfig) -> PlanGoverned<AdminRepo> {
    let repo = AdminRepo::new(pool.clone(), None, "plan_governance_test");
    let quotas = Arc::new(QuotaService::new(cfg, Some(pool.clone())));
    PlanGoverned::new(Arc::new(repo), quotas)
}

fn interval_of(targets: &[(OrgId, Target)], id: Uuid) -> Duration {
    targets
        .iter()
        .find(|(_, t)| t.id == id)
        .map(|(_, t)| t.interval)
        .expect("target present in the scheduler set")
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_monitor_below_its_plan_floor_is_slowed_for_the_scheduler() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "free", 60).await;

    let listed = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");
    assert_eq!(interval_of(&listed, target), Duration::from_secs(180));

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn governing_never_rewrites_the_stored_interval() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "free", 60).await;

    let _ = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");

    let (stored,): (i32,) = sqlx::query_as("SELECT interval_secs FROM targets WHERE id = $1")
        .bind(target)
        .fetch_one(&pool)
        .await
        .expect("stored interval");
    assert_eq!(stored, 60, "the clamp must not write back");

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_monitor_above_its_plan_floor_is_left_alone() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "team", 60).await;

    let listed = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");
    assert_eq!(interval_of(&listed, target), Duration::from_secs(60));

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_downgrade_takes_effect_without_touching_the_monitor() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "team", 60).await;

    let before = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");
    assert_eq!(interval_of(&before, target), Duration::from_secs(60));

    sqlx::query(
        "UPDATE accounts SET plan_id = 'free' \
         WHERE id = (SELECT account_id FROM organizations WHERE id = $1)",
    )
    .bind(org.0)
    .execute(&pool)
    .await
    .expect("downgrade");

    let after = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");
    assert_eq!(interval_of(&after, target), Duration::from_secs(180));

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn the_incident_writer_walks_the_same_slowed_interval_as_the_scheduler() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "free", 30).await;

    let source = governed_source(&pool, &cfg);
    let mut cursor = None;
    let mut paged = Vec::new();
    loop {
        let page = source
            .next_enabled_target_page(cursor, 500)
            .await
            .expect("page");
        let Some((last_org, last)) = page.last() else {
            break;
        };
        cursor = Some(uptimepage::storage::admin::PublicTargetCursor::after(
            *last_org, last.id,
        ));
        paged.extend(page);
    }
    assert_eq!(interval_of(&paged, target), Duration::from_secs(180));

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn an_override_lifts_the_floor_for_one_account() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "free", 60).await;

    sqlx::query(
        "INSERT INTO plan_overrides (account_id, override_json, reason) \
         SELECT account_id, '{\"min_check_interval_secs\": 30}'::jsonb, 'test' \
           FROM organizations WHERE id = $1 \
         ON CONFLICT (account_id) DO UPDATE SET override_json = excluded.override_json",
    )
    .bind(org.0)
    .execute(&pool)
    .await
    .expect("override");

    let listed = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");
    assert_eq!(interval_of(&listed, target), Duration::from_secs(60));

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn the_region_etag_changes_when_the_plan_does() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let region = unique_slug("reg");
    let (org, target, user) = org_on_plan(&pool, "team", 60).await;
    sqlx::query("INSERT INTO regions (id, name, enabled) VALUES ($1, $1, true)")
        .bind(&region)
        .execute(&pool)
        .await
        .expect("region");
    sqlx::query("INSERT INTO target_regions (target_id, region) VALUES ($1, $2)")
        .bind(target)
        .bind(&region)
        .execute(&pool)
        .await
        .expect("assign region");

    let repo = AdminRepo::new(pool.clone(), None, "plan_governance_test");
    let before = repo
        .region_pull_etag(&region, &plan_digest_for(&pool, &region).await)
        .await
        .expect("etag");

    sqlx::query(
        "UPDATE accounts SET plan_id = 'free' \
         WHERE id = (SELECT account_id FROM organizations WHERE id = $1)",
    )
    .bind(org.0)
    .execute(&pool)
    .await
    .expect("downgrade");

    let after = repo
        .region_pull_etag(&region, &plan_digest_for(&pool, &region).await)
        .await
        .expect("etag");
    assert_ne!(
        before, after,
        "a plan change must invalidate the region pull"
    );

    cleanup(&pool, org, user).await;
    let _ = sqlx::query("DELETE FROM regions WHERE id = $1")
        .bind(&region)
        .execute(&pool)
        .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn the_region_etag_changes_when_the_plans_floor_is_retuned() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let region = unique_slug("reg");
    // Its own plan row: retuning a seeded tier would race every other suite
    // sharing this database.
    let plan = unique_slug("pl").replace('-', "_");
    sqlx::query(
        "INSERT INTO plans (id, name, description, max_orgs, max_targets, \
         min_check_interval_secs, retention_days, max_members, \
         max_pending_invitations, max_api_tokens_per_user, max_public_components, \
         max_status_pages, max_share_links_per_monitor, max_shared_monitors, \
         max_maintenance_windows, max_notification_channels, max_logo_size_bytes, \
         api_writes_per_minute, api_reads_per_minute, bulk_ops_per_minute, \
         test_now_per_minute, check_now_per_minute) \
         VALUES ($1, 'Throwaway', 'test', 1, 1, 30, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, \
                 1, 1, 1, 1, 1)",
    )
    .bind(&plan)
    .execute(&pool)
    .await
    .expect("throwaway plan");
    let (org, target, user) = org_on_plan(&pool, &plan, 60).await;
    sqlx::query("INSERT INTO regions (id, name, enabled) VALUES ($1, $1, true)")
        .bind(&region)
        .execute(&pool)
        .await
        .expect("region");
    sqlx::query("INSERT INTO target_regions (target_id, region) VALUES ($1, $2)")
        .bind(target)
        .bind(&region)
        .execute(&pool)
        .await
        .expect("assign region");

    let repo = AdminRepo::new(pool.clone(), None, "plan_governance_test");
    let before = repo
        .region_pull_etag(&region, &plan_digest_for(&pool, &region).await)
        .await
        .expect("etag");

    sqlx::query("UPDATE plans SET min_check_interval_secs = 45 WHERE id = $1")
        .bind(&plan)
        .execute(&pool)
        .await
        .expect("retune");
    let after = repo
        .region_pull_etag(&region, &plan_digest_for(&pool, &region).await)
        .await
        .expect("etag");

    assert_ne!(before, after, "a retuned floor must invalidate the pull");

    cleanup(&pool, org, user).await;
    let _ = sqlx::query("DELETE FROM regions WHERE id = $1")
        .bind(&region)
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM plans WHERE id = $1")
        .bind(&plan)
        .execute(&pool)
        .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_plan_without_sms_refuses_a_text_message_channel() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |cfg| cfg.marketing.enabled = true).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/notification-channels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "pager",
                        "config": {
                            "type": "sms",
                            "provider": "twilio",
                            "to": "+15551234567",
                            "account_sid": "AC00000000000000000000000000000000",
                            "auth_token": "tok",
                            "from": "+15559876543"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("request");

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "SMS_ALERTS_DISABLED");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_heartbeat_keeps_its_own_interval_under_a_slower_plan() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let cfg = AppConfig::load().expect("config");
    let (org, target, user) = org_on_plan(&pool, "free", 60).await;
    sqlx::query("UPDATE targets SET check_spec = $2 WHERE id = $1")
        .bind(target)
        .bind(
            serde_json::to_value(CheckSpec::Heartbeat(HeartbeatCheck {
                period: Duration::from_secs(300),
                grace: Duration::from_secs(60),
                max_runtime: None,
            }))
            .expect("check spec"),
        )
        .execute(&pool)
        .await
        .expect("make heartbeat");

    let listed = governed_source(&pool, &cfg)
        .list_all_enabled_targets()
        .await
        .expect("list");
    assert_eq!(interval_of(&listed, target), Duration::from_secs(60));

    cleanup(&pool, org, user).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_plan_without_sms_refuses_a_patch_into_a_text_message_channel() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |cfg| cfg.marketing.enabled = true).await;

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/notification-channels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": unique_slug("hook"),
                        "config": {"type": "webhook", "url": "https://example.com/hook"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = body_json(created).await["id"].as_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/notification-channels/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "config": {
                            "type": "sms",
                            "provider": "twilio",
                            "to": "+15551234567",
                            "account_sid": "AC00000000000000000000000000000000",
                            "auth_token": "tok",
                            "from": "+15559876543"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("patch");

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "SMS_ALERTS_DISABLED");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_self_hosted_install_may_add_a_text_message_channel() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (app, _org) = build_test_app_with_pg_store(pool, |cfg| cfg.marketing.enabled = false).await;

    let resp = app
        .oneshot(sms_create_request(&unique_slug("sms")))
        .await
        .expect("create");

    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "the plans row is the operator's own on a self-hosted install"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn an_existing_text_message_channel_stays_editable_after_a_downgrade() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // One org on a plan without SMS, holding a channel from when it had one:
    // seeded directly, because the create path is exactly what now refuses it.
    let (app, org) = build_test_app_with_pg_store(pool.clone(), |cfg| {
        cfg.marketing.enabled = true;
    })
    .await;
    let name = unique_slug("sms");
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO notification_channels (org_id, name, kind, config, verified_at) \
         VALUES ($1, $2, 'sms', $3, now()) RETURNING id",
    )
    .bind(org.0)
    .bind(&name)
    .bind(sms_config("+15551234567", "leaked"))
    .fetch_one(&pool)
    .await
    .expect("seed the channel the plan no longer grants");

    // Replacing the config is the only path the gate sees; a rename never
    // reaches it. Rotating a leaked token must not need the plan back.
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/notification-channels/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "config": sms_config("+15557654321", "rotated") })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("patch");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a stored SMS channel must stay correctable once the plan drops SMS"
    );

    let (stored,): (serde_json::Value,) =
        sqlx::query_as("SELECT config FROM notification_channels WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("read back");
    assert_eq!(stored["to"], "+15557654321", "the edit must have landed");

    sqlx::query("DELETE FROM notification_channels WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_plan_without_sms_refuses_a_test_send_of_one() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // The rehearsal for a create the plan would refuse has to refuse too, or
    // the entitlement is decorative: the send goes out all the same.
    let (app, _org) = build_test_app_with_pg_store(pool, |cfg| cfg.marketing.enabled = true).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/notification-channels/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "config": sms_config("+15551234567", "tok") }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("test send");

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "SMS_ALERTS_DISABLED");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn a_patch_to_an_unknown_channel_is_not_found_not_forbidden() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    // The gate must not answer ahead of the lookup, or a caller learns the
    // plan from an id that was never theirs.
    let (app, _org) = build_test_app_with_pg_store(pool, |cfg| cfg.marketing.enabled = true).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/notification-channels/{}", Uuid::now_v7()))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "config": sms_config("+15551234567", "tok") }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("patch");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn the_floor_only_ever_slows_a_monitor_down() {
    let mut plan = uptimepage::domain::quota::Plan {
        min_check_interval_secs: 120,
        ..plan_fixture()
    };
    assert_eq!(
        governed_interval(Duration::from_secs(30), &plan, "http"),
        Duration::from_secs(120)
    );
    plan.min_check_interval_secs = 10;
    assert_eq!(
        governed_interval(Duration::from_secs(30), &plan, "http"),
        Duration::from_secs(30)
    );
}

fn plan_fixture() -> uptimepage::domain::quota::Plan {
    let now = chrono::Utc::now();
    uptimepage::domain::quota::Plan {
        id: "fixture".into(),
        name: "Fixture".into(),
        description: String::new(),
        max_targets: 1,
        min_check_interval_secs: 1,
        retention_days: 1,
        raw_days: 1,
        evidence_days: 1,
        max_members: 1,
        max_pending_invitations: 1,
        max_api_tokens_per_user: 1,
        max_public_components: 1,
        max_status_pages: 1,
        max_share_links_per_monitor: 1,
        max_shared_monitors: 1,
        max_maintenance_windows: 1,
        max_notification_channels: 1,
        max_escalation_policies: 1,
        max_on_call_schedules: 1,
        max_logo_size_bytes: 1,
        max_regions: 1,
        max_orgs: 1,
        api_writes_per_minute: 1,
        api_reads_per_minute: 1,
        bulk_ops_per_minute: 1,
        test_now_per_minute: 1,
        check_now_per_minute: 1,
        custom_domain_enabled: false,
        white_label_enabled: false,
        sms_alerts_enabled: false,
        incident_narration_enabled: true,
        on_call_enabled: true,
        max_flow_checks: 0,
        max_flow_steps: 30,
        is_listed: false,
        created_at: now,
        updated_at: now,
    }
}
