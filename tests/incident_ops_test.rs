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
use uptimepage::domain::{
    IncidentEventKind, IncidentState, NewIncidentNotification, NewManualIncident,
    NotificationReason, NotificationStatus, OrgId,
};
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

#[tokio::test]
#[ignore]
async fn notification_log_record_retry_and_scope_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, _user, id) = seed(&pool, "incnotif").await;
    let store = PgIncidentOpsStore::new(pool.clone());

    // A failed paging row (channel_id NULL avoids needing a real channel; the
    // org-match trigger still validates incident_id ↔ org_id).
    store
        .record_notification(NewIncidentNotification {
            org,
            incident_id: id,
            escalation_level: Some(0),
            target_user_id: None,
            channel_id: None,
            transport: "slack".into(),
            reason: NotificationReason::Opened,
            status: NotificationStatus::Failed,
            attempt: 1,
            error: Some("connect refused".into()),
            sent_at: None,
        })
        .await
        .expect("record");

    let rows = store.notifications_for(org, id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, NotificationReason::Opened);
    assert_eq!(rows[0].status, NotificationStatus::Failed);
    let notif_id = rows[0].id;

    // The retry scan picks it up (attempt 1 < cap) with the owning org.
    let pending = store.pending_notifications(50, 5).await.unwrap();
    let mine = pending.iter().find(|p| p.id == notif_id).expect("pending");
    assert_eq!(mine.org, org);
    assert_eq!(mine.reason, NotificationReason::Opened);

    // Mark it sent; it must drop out of the retry scan.
    store
        .mark_notification(org, notif_id, NotificationStatus::Sent, 2, None, Some(chrono::Utc::now()))
        .await
        .unwrap();
    let rows = store.notifications_for(org, id).await.unwrap();
    assert_eq!(rows[0].status, NotificationStatus::Sent);
    assert_eq!(rows[0].attempt, 2);
    assert!(rows[0].sent_at.is_some());
    assert!(
        store
            .pending_notifications(50, 5)
            .await
            .unwrap()
            .iter()
            .all(|p| p.id != notif_id),
        "a sent row must not be retried"
    );

    // append_event lands on the timeline; cross-org reads see nothing.
    store
        .append_event(org, id, IncidentEventKind::Notified, Actor::System, Some("paged".into()))
        .await
        .unwrap();
    let tl = store.timeline(org, id).await.unwrap();
    assert!(tl.iter().any(|e| e.kind == IncidentEventKind::Notified));

    let other_user = make_user(&pool, "incnotifx").await;
    let other = create_org_with_owner(&pool, other_user, &unique_slug("incnotifx"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store.notifications_for(other.id, id).await.unwrap().is_empty(),
        "another org must not see this incident's paging log"
    );
}

#[tokio::test]
#[ignore]
async fn publish_sets_visibility_and_narration_then_unpublish_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, user, id) = seed(&pool, "incpub").await;
    let store = PgIncidentOpsStore::new(pool.clone());

    // Publish seeds the public title; visibility flips to public.
    let pubd = store
        .publish(org, id, Some("EU API outage".into()), None, Actor::User(user))
        .await
        .unwrap()
        .expect("incident exists");
    assert_eq!(pubd.visibility, uptimepage::domain::IncidentVisibility::Public);

    let (vis, title): (String, Option<String>) =
        sqlx::query_as("SELECT visibility, public_title FROM incidents WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(vis, "public");
    assert_eq!(title.as_deref(), Some("EU API outage"));

    // A second publish without a title must not clobber the stored copy.
    store.publish(org, id, None, None, Actor::User(user)).await.unwrap().unwrap();
    let kept: Option<String> = sqlx::query_scalar("SELECT public_title FROM incidents WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept.as_deref(), Some("EU API outage"));

    let unpubd = store.unpublish(org, id, Actor::User(user)).await.unwrap().unwrap();
    assert_eq!(unpubd.visibility, uptimepage::domain::IncidentVisibility::Internal);

    let tl = store.timeline(org, id).await.unwrap();
    assert!(tl.iter().any(|e| e.kind == IncidentEventKind::Published));
    assert!(tl.iter().any(|e| e.kind == IncidentEventKind::Unpublished));

    // Cross-tenant publish is a no-op (returns None, leaves the row internal).
    let other_user = make_user(&pool, "incpubx").await;
    let other = create_org_with_owner(&pool, other_user, &unique_slug("incpubx"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store.publish(other.id, id, None, None, Actor::User(other_user)).await.unwrap().is_none(),
        "another org cannot publish this incident"
    );
}

#[tokio::test]
#[ignore]
async fn metrics_rolls_up_mtta_mttr_and_counts_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, user, a) = seed(&pool, "incmetrics").await;
    let store = PgIncidentOpsStore::new(pool.clone());

    // A: human-acknowledged then human-resolved (contributes MTTA + MTTR).
    store.acknowledge(org, a, Actor::User(user), None).await.unwrap();
    store.resolve(org, a, Actor::User(user), None).await.unwrap();

    // B: manual incident auto-resolved by the system (no resolver).
    let b = store
        .declare(org, NewManualIncident::default(), Actor::User(user))
        .await
        .unwrap();
    store.auto_resolve(org, b.id).await.unwrap();

    // C: manual incident left triggered.
    store
        .declare(org, NewManualIncident::default(), Actor::User(user))
        .await
        .unwrap();

    let m = store.metrics(org, 30).await.unwrap();
    assert_eq!(m.window_days, 30);
    assert_eq!(m.total, 3);
    assert!(m.mtta_secs.is_some(), "A was acknowledged");
    assert!(m.mttr_secs.is_some(), "A and B were resolved");
    assert_eq!(m.auto_resolved, 1, "B auto-resolved");
    assert_eq!(m.human_resolved, 1, "A human-resolved");
    let resolved = m.by_state.iter().find(|b| b.key == "resolved").map(|b| b.count);
    assert_eq!(resolved, Some(2));
    // A carries a target; the noisy-monitor list surfaces it.
    assert!(m.top_monitors.iter().any(|t| t.count >= 1));

    // A short window that predates every incident yields zeroes, not an error.
    // (All incidents are seconds old, so window_days=1 still includes them; the
    // store clamps and never divides by zero on an empty set.)
    let other = store.metrics(org, 1).await.unwrap();
    assert_eq!(other.total, 3);

    // Cross-tenant isolation: a fresh org sees none of these.
    let other_user = make_user(&pool, "incmetricsx").await;
    let other_org = create_org_with_owner(&pool, other_user, &unique_slug("incmetricsx"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    let isolated = store.metrics(other_org.id, 30).await.unwrap();
    assert_eq!(isolated.total, 0);
    assert!(isolated.mtta_secs.is_none());
    assert!(isolated.mttr_secs.is_none());
}

#[tokio::test]
#[ignore]
async fn postmortem_publish_events_persist_with_actor_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, user, id) = seed(&pool, "incpmev").await;
    let store = PgIncidentOpsStore::new(pool.clone());

    // These kinds are part of the incident_events CHECK; append_event must
    // accept them and record the acting user.
    store
        .append_event(org, id, IncidentEventKind::PostmortemPublished, Actor::User(user), None)
        .await
        .unwrap();
    store
        .append_event(org, id, IncidentEventKind::PostmortemUnpublished, Actor::User(user), None)
        .await
        .unwrap();

    let tl = store.timeline(org, id).await.unwrap();
    assert!(
        tl.iter().any(|e| e.kind == IncidentEventKind::PostmortemPublished
            && e.actor_id == Some(user)),
        "postmortem publish is attributed on the timeline"
    );
    assert!(
        tl.iter()
            .any(|e| e.kind == IncidentEventKind::PostmortemUnpublished),
        "postmortem unpublish lands on the timeline"
    );
}
