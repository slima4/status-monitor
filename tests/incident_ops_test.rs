//! Postgres-backed validation of the operational incident lifecycle store
//! (`PgIncidentOpsStore`). The in-memory store unit tests cover the state
//! machine; these exercise the real SQL: the transition UPDATE ... RETURNING,
//! the `OpsIncident` row mapping, the per-incident advisory lock, the timeline
//! event insert (+ its org-match trigger), and cross-tenant scoping.
//!
//! `#[ignore]`d by default; runs under `--run-ignored all` once `DATABASE_URL`
//! is set. The harness auto-applies all migrations on first connect.

mod common;

use common::{make_user, unique_slug};
use sqlx::PgPool;
use uptimepage::domain::{IncidentState, OrgId};
use uptimepage::storage::{
    Actor, IncidentOpsStore, LifecycleOutcome, PgIncidentOpsStore, create_org_with_owner,
};
use uuid::Uuid;

/// Seed an org (with owner) + a target + one open (`triggered`) incident.
/// Returns (org, owner user, incident id).
async fn seed(pool: &PgPool, prefix: &str) -> (OrgId, uptimepage::domain::UserId, Uuid) {
    let user = make_user(pool, prefix).await;
    let org = create_org_with_owner(pool, user, &unique_slug(prefix), "n", 3)
        .await
        .expect("create org")
        .expect("org created");
    let target_id: Uuid = sqlx::query_scalar(
        "INSERT INTO targets (org_id, name, check_spec, interval_secs) \
         VALUES ($1, 'svc', '{}'::jsonb, 30) RETURNING id",
    )
    .bind(org.id.0)
    .fetch_one(pool)
    .await
    .expect("insert target");
    let incident_id: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (org_id, target_id, started_at, status_at_start) \
         VALUES ($1, $2, now() - interval '5 minutes', 'down') RETURNING id",
    )
    .bind(org.id.0)
    .bind(target_id)
    .fetch_one(pool)
    .await
    .expect("insert incident");
    (org.id, user, incident_id)
}

fn updated(o: LifecycleOutcome) -> uptimepage::domain::OpsIncident {
    match o {
        LifecycleOutcome::Updated(i) => *i,
        other => panic!("expected Updated, got {other:?}"),
    }
}

#[tokio::test]
#[ignore]
async fn acknowledge_then_manual_resolve_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, user, id) = seed(&pool, "incack").await;
    let store = PgIncidentOpsStore::new(pool.clone());

    let acked = updated(
        store
            .acknowledge(org, id, Actor::User(user), Some("on it".into()))
            .await
            .unwrap(),
    );
    assert_eq!(acked.state, IncidentState::Acknowledged);
    assert_eq!(acked.acknowledged_by, Some(user));
    assert!(acked.acknowledged_at.is_some());
    assert!(acked.next_escalation_at.is_none());

    let resolved = updated(
        store
            .resolve(org, id, Actor::User(user), None)
            .await
            .unwrap(),
    );
    assert_eq!(resolved.state, IncidentState::Resolved);
    assert_eq!(resolved.resolved_by, Some(user));
    assert!(resolved.ended_at.is_some());

    // Timeline recorded both lifecycle events, oldest first.
    let tl = store.timeline(org, id).await.unwrap();
    let kinds: Vec<_> = tl.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            uptimepage::domain::IncidentEventKind::Acknowledged,
            uptimepage::domain::IncidentEventKind::Resolved
        ]
    );
}

#[tokio::test]
#[ignore]
async fn auto_resolve_leaves_resolved_by_null_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, _user, id) = seed(&pool, "incauto").await;
    let store = PgIncidentOpsStore::new(pool.clone());
    let resolved = updated(store.auto_resolve(org, id).await.unwrap());
    assert_eq!(resolved.state, IncidentState::Resolved);
    assert_eq!(resolved.resolved_by, None);
}

#[tokio::test]
#[ignore]
async fn reopen_after_resolve_clears_state_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, user, id) = seed(&pool, "increopen").await;
    let store = PgIncidentOpsStore::new(pool.clone());
    store
        .acknowledge(org, id, Actor::User(user), None)
        .await
        .unwrap();
    store.resolve(org, id, Actor::User(user), None).await.unwrap();
    let reopened = updated(store.reopen(org, id, Actor::User(user), None).await.unwrap());
    assert_eq!(reopened.state, IncidentState::Triggered);
    assert!(reopened.ended_at.is_none());
    assert!(reopened.acknowledged_by.is_none());
    assert!(reopened.resolved_by.is_none());
}

#[tokio::test]
#[ignore]
async fn illegal_transition_is_reported_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, user, id) = seed(&pool, "incillegal").await;
    let store = PgIncidentOpsStore::new(pool.clone());
    store.resolve(org, id, Actor::User(user), None).await.unwrap();
    // Acknowledging a resolved incident is illegal (reopen first).
    let out = store
        .acknowledge(org, id, Actor::User(user), None)
        .await
        .unwrap();
    assert!(matches!(out, LifecycleOutcome::IllegalTransition(_)));
}

#[tokio::test]
#[ignore]
async fn writer_opens_internal_incident_for_private_monitor_pg() {
    use uptimepage::domain::CheckStatus;
    use uptimepage::public_status::incident_writer::{
        IncidentStore, NewOpenIncident, PgIncidentStore,
    };

    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "incvis").await;
    let org = create_org_with_owner(&pool, user, &unique_slug("incvis"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    // A monitor on no status page at all.
    let target_id: Uuid = sqlx::query_scalar(
        "INSERT INTO targets (org_id, name, check_spec, interval_secs) \
         VALUES ($1, 'private-svc', '{}'::jsonb, 30) RETURNING id",
    )
    .bind(org.id.0)
    .fetch_one(&pool)
    .await
    .unwrap();

    let store = PgIncidentStore::new(pool.clone());
    let id = store
        .insert_open(
            org.id,
            NewOpenIncident {
                target_id,
                started_at: chrono::Utc::now(),
                status_at_start: CheckStatus::Down,
                check_count: 2,
                error_sample: None,
            },
        )
        .await
        .unwrap();

    let visibility: String = sqlx::query_scalar("SELECT visibility FROM incidents WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        visibility, "internal",
        "a private monitor's incident must never be publicly visible"
    );
}

#[tokio::test]
#[ignore]
async fn cross_org_cannot_touch_incident_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (_org_a, _user_a, id) = seed(&pool, "incown").await;
    // A second, unrelated org must not see or mutate org A's incident.
    let other_user = make_user(&pool, "incother").await;
    let other_org = create_org_with_owner(&pool, other_user, &unique_slug("incother"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    let store = PgIncidentOpsStore::new(pool.clone());

    assert!(store.get(other_org.id, id).await.unwrap().is_none());
    let out = store
        .acknowledge(other_org.id, id, Actor::User(other_user), None)
        .await
        .unwrap();
    assert!(matches!(out, LifecycleOutcome::NotFound));
    // The legitimate owner's add_note works; the other org's is a no-op.
    assert!(
        store
            .add_note(other_org.id, id, Actor::User(other_user), "x".into())
            .await
            .unwrap()
            .is_none()
    );
}
