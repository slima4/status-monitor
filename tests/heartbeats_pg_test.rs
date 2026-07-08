//! Live-Postgres contract for `heartbeat_monitors`: idempotent token minting,
//! single-statement ping recording (unknown / deleted-target / deleted-org →
//! None), the store-level disabled→enabled re-arm, per-org isolation, the
//! refresh-time row self-heal, and migration 031.
//!
//! Live-PG ignored: needs `DATABASE_URL`. Migrations auto-apply on first
//! connect. Point it at a throwaway DB to also validate migration 031.

mod common;

use std::time::Duration;

use uptimepage::domain::{
    CheckSpec, ExpectedStatus, HeartbeatCheck, NewTarget, OrgId, TargetUpdate, UserId, WriteSource,
};
use uptimepage::storage::admin::AdminRepo;
use uptimepage::storage::{
    HeartbeatStore, PgHeartbeatStore, PostgresTargetStore, RestoreOutcome, TargetStore,
    create_org_with_owner, restore_org, soft_delete_org,
};
use uuid::Uuid;

use common::{default_http_check, make_user, pg_pool_from_env, unique_slug};

async fn two_orgs(pool: &sqlx::PgPool, tag: &str) -> (OrgId, OrgId, UserId, UserId) {
    let user_a = make_user(pool, tag).await;
    let user_b = make_user(pool, tag).await;
    let org_a = create_org_with_owner(pool, user_a, &unique_slug(tag), "A", 3)
        .await
        .unwrap()
        .expect("org a")
        .id;
    let org_b = create_org_with_owner(pool, user_b, &unique_slug(tag), "B", 3)
        .await
        .unwrap()
        .expect("org b")
        .id;
    (org_a, org_b, user_a, user_b)
}

fn heartbeat_target(name: &str, enabled: bool) -> NewTarget {
    NewTarget {
        name: name.into(),
        check: CheckSpec::Heartbeat(HeartbeatCheck {
            period: Duration::from_secs(300),
            grace: Duration::from_secs(60),
        }),
        interval: Duration::from_secs(60),
        enabled,
        tags: vec![],
        alerts: Default::default(),
        region_policy: Default::default(),
        alert_confirmations: 2,
        notify_recovery: true,
        renotify_interval_secs: 3600,
        group_name: None,
        owner_user_id: None,
    }
}

async fn make_heartbeat_target(pool: &sqlx::PgPool, org: OrgId, name: &str, enabled: bool) -> Uuid {
    let store = PostgresTargetStore::from_pool(pool.clone(), None);
    store
        .create(
            org,
            heartbeat_target(name, enabled),
            WriteSource::Ui,
            i64::MAX,
            i64::MAX,
        )
        .await
        .expect("create target")
        .id
}

async fn cleanup(pool: &sqlx::PgPool, orgs: &[OrgId], users: &[UserId]) {
    let _ = sqlx::query("DELETE FROM organizations WHERE id = ANY($1)")
        .bind(orgs.iter().map(|o| o.0).collect::<Vec<_>>())
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(users.iter().map(|u| u.0).collect::<Vec<_>>())
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run via DATABASE_URL=... cargo test -- --ignored"]
async fn ensure_mints_once_and_ping_round_trips_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "hb-rt").await;
    let store = PgHeartbeatStore::new(pool.clone(), None);
    let target = make_heartbeat_target(&pool, org_a, "backup", true).await;

    let first = store.ensure(org_a, target).await.unwrap().expect("row");
    let token = first.token.clone().expect("plaintext without cipher");
    assert!(first.last_ping_at.is_none());
    assert_eq!(first.anchor(), first.armed_at, "no ping yet → armed_at");

    // Repeated ensure keeps the same token; a foreign org can't mint or read.
    let again = store.ensure(org_a, target).await.unwrap().expect("row");
    assert_eq!(again.token, first.token);
    assert!(store.ensure(org_b, target).await.unwrap().is_none());
    assert!(store.get(org_b, target).await.unwrap().is_none());

    // One-statement ping: resolves, records, and reads back.
    let (pinged, at) = store
        .record_ping_by_token(&token)
        .await
        .unwrap()
        .expect("token records");
    assert_eq!(pinged, target);
    let read = store.get(org_a, target).await.unwrap().expect("row");
    assert_eq!(read.last_ping_at, Some(at));
    assert_eq!(read.anchor(), at, "ping newer than arm point wins");
    assert!(store.record_ping_by_token("nope").await.unwrap().is_none());

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run via DATABASE_URL=... cargo test -- --ignored"]
async fn enable_rearms_only_disabled_heartbeats_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "hb-arm").await;
    let store = PgHeartbeatStore::new(pool.clone(), None);
    let targets = PostgresTargetStore::from_pool(pool.clone(), None);
    let paused = make_heartbeat_target(&pool, org_a, "paused", false).await;
    let running = make_heartbeat_target(&pool, org_a, "running", true).await;
    let armed_at = |org, id| {
        let store = &store;
        async move { store.get(org, id).await.unwrap().unwrap() }
    };
    store.ensure(org_a, paused).await.unwrap();
    store.ensure(org_a, running).await.unwrap();
    let paused_before = armed_at(org_a, paused).await;
    let running_before = armed_at(org_a, running).await;

    // A foreign org's enable can't re-arm across the tenant boundary.
    targets.set_enabled(org_b, &[paused], true).await.unwrap();
    assert_eq!(
        armed_at(org_a, paused).await.armed_at,
        paused_before.armed_at
    );

    // set_enabled re-arms the disabled→enabled flip only, and the re-arm is
    // NOT a fabricated ping.
    targets
        .set_enabled(org_a, &[paused, running], true)
        .await
        .unwrap();
    let paused_after = armed_at(org_a, paused).await;
    assert!(paused_after.armed_at > paused_before.armed_at);
    assert!(paused_after.last_ping_at.is_none());
    assert_eq!(
        armed_at(org_a, running).await.armed_at,
        running_before.armed_at,
        "already-enabled monitors keep their arm point"
    );

    // The PATCH-style update path shares the same contract.
    targets.set_enabled(org_a, &[paused], false).await.unwrap();
    let re_paused = armed_at(org_a, paused).await;
    targets
        .update(
            org_a,
            paused,
            TargetUpdate {
                enabled: Some(true),
                ..Default::default()
            },
            WriteSource::Ui,
        )
        .await
        .unwrap();
    assert!(armed_at(org_a, paused).await.armed_at > re_paused.armed_at);

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run via DATABASE_URL=... cargo test -- --ignored"]
async fn dead_tokens_stop_recording_and_sync_heals_rows_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "hb-del").await;
    let store = PgHeartbeatStore::new(pool.clone(), None);
    let targets = PostgresTargetStore::from_pool(pool.clone(), None);

    // Target delete cascades the row.
    let doomed = make_heartbeat_target(&pool, org_a, "doomed", true).await;
    let tok_doomed = store
        .ensure(org_a, doomed)
        .await
        .unwrap()
        .unwrap()
        .token
        .unwrap();
    assert!(targets.delete(org_a, doomed).await.unwrap());
    assert!(
        store
            .record_ping_by_token(&tok_doomed)
            .await
            .unwrap()
            .is_none()
    );

    // Kind switch away: remove() revokes the token.
    let switched = make_heartbeat_target(&pool, org_a, "switched", true).await;
    let tok_switched = store
        .ensure(org_a, switched)
        .await
        .unwrap()
        .unwrap()
        .token
        .unwrap();
    let url = url::Url::parse("https://example.com/").unwrap();
    targets
        .update(
            org_a,
            switched,
            TargetUpdate {
                check: Some(CheckSpec::Http(default_http_check(
                    url,
                    ExpectedStatus::Exact(200),
                ))),
                ..Default::default()
            },
            WriteSource::Ui,
        )
        .await
        .unwrap();
    assert!(store.remove(org_a, switched).await.unwrap());
    assert!(
        store
            .record_ping_by_token(&tok_switched)
            .await
            .unwrap()
            .is_none()
    );

    // Refresh-time self-heal: a heartbeat target with a lost row gets one
    // minted, and its anchor shows up in the snapshot.
    let healed = make_heartbeat_target(&pool, org_a, "healed", true).await;
    sqlx::query("DELETE FROM heartbeat_monitors WHERE target_id = $1")
        .bind(healed)
        .execute(&pool)
        .await
        .unwrap();
    let repo = AdminRepo::new(pool.clone(), None, "heartbeat_refresh");
    let anchors = repo.sync_heartbeat_rows().await.unwrap();
    assert!(anchors.iter().any(|(id, _)| *id == healed));
    assert!(store.get(org_a, healed).await.unwrap().is_some());

    // Soft-deleted org stops recording without dropping the row, and its
    // anchors leave the snapshot.
    let orphan = make_heartbeat_target(&pool, org_b, "orphan", true).await;
    let tok_orphan = store
        .ensure(org_b, orphan)
        .await
        .unwrap()
        .unwrap()
        .token
        .unwrap();
    soft_delete_org(&pool, org_b, user_b).await.unwrap();
    assert!(
        store
            .record_ping_by_token(&tok_orphan)
            .await
            .unwrap()
            .is_none()
    );
    let anchors = repo.sync_heartbeat_rows().await.unwrap();
    assert!(anchors.iter().all(|(id, _)| *id != orphan));

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run via DATABASE_URL=... cargo test -- --ignored"]
async fn restore_org_rearms_heartbeats_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "hb-restore").await;
    let store = PgHeartbeatStore::new(pool.clone(), None);
    let target = make_heartbeat_target(&pool, org_a, "cron", true).await;
    store.ensure(org_a, target).await.unwrap();

    // Freeze the anchor two hours back: after a deletion window this is stale
    // enough that an un-armed monitor would false-Down on the first eval.
    sqlx::query(
        "UPDATE heartbeat_monitors \
         SET armed_at = now() - INTERVAL '2 hours', last_ping_at = now() - INTERVAL '2 hours' \
         WHERE target_id = $1",
    )
    .bind(target)
    .execute(&pool)
    .await
    .unwrap();

    soft_delete_org(&pool, org_a, user_a).await.unwrap();
    let outcome = restore_org(&pool, org_a, user_a, 30).await.unwrap();
    assert!(matches!(outcome, RestoreOutcome::Restored(_)));

    let hb = store.get(org_a, target).await.unwrap().expect("row");
    let arm_age = chrono::Utc::now().signed_duration_since(hb.armed_at);
    assert!(
        arm_age.num_seconds() < 60,
        "restore must re-arm; armed_at age {}s",
        arm_age.num_seconds()
    );
    // The re-arm is not a fabricated ping: real ping history stays put.
    let ping = hb.last_ping_at.expect("backdated ping");
    assert!(ping < chrono::Utc::now() - chrono::Duration::minutes(30));
    assert_eq!(hb.anchor(), hb.armed_at, "fresh arm point wins the anchor");

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run via DATABASE_URL=... cargo test -- --ignored"]
async fn cross_tenant_row_insert_is_blocked_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "hb-xtenant").await;
    let target = make_heartbeat_target(&pool, org_a, "t", true).await;

    // A raw insert binding org_a's target under org_b trips the org-match
    // trigger, so a request-supplied target_id can't cross tenants.
    let res = sqlx::query(
        "INSERT INTO heartbeat_monitors (target_id, org_id, token_hash, token_enc) \
         VALUES ($1, $2, 'x-hash', 'x-enc')",
    )
    .bind(target)
    .bind(org_b.0)
    .execute(&pool)
    .await;
    assert!(
        res.is_err(),
        "cross-tenant heartbeat insert must be rejected"
    );

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run via DATABASE_URL=... cargo test -- --ignored"]
async fn ensure_after_ping_preserves_state_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "hb-keep").await;
    let store = PgHeartbeatStore::new(pool.clone(), None);
    let target = make_heartbeat_target(&pool, org_a, "keep", true).await;
    let token = store
        .ensure(org_a, target)
        .await
        .unwrap()
        .unwrap()
        .token
        .unwrap();
    let (_, at) = store
        .record_ping_by_token(&token)
        .await
        .unwrap()
        .expect("ping records");

    // A re-save hits the existence check, so it neither re-mints the token nor
    // clobbers the recorded ping.
    let again = store.ensure(org_a, target).await.unwrap().expect("row");
    assert_eq!(again.token.as_deref(), Some(token.as_str()));
    assert_eq!(again.last_ping_at, Some(at));

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}
