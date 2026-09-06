//! Postgres-backed validation of `PgOnCallStore`, `PgContactStore`, and
//! migration 020. The in-memory unit tests cover the resolver logic; these
//! exercise the real SQL: nested layer+participant inserts, the parent-chain
//! org-match triggers, the member-validation IDOR guard, override CRUD, the
//! `escalation_targets.schedule_id` FK wired by 020, contact-channel ownership,
//! and cross-tenant scoping.
//!
//! `#[ignore]`d by default; runs under `--run-ignored all` once `DATABASE_URL`
//! is set. A clean run is also the fresh-DB validation of `020_on_call`.

mod common;

use common::{make_user, unique_slug};
use sqlx::PgPool;
use uptimepage::domain::{
    NewOnCallLayer, NewOnCallOverride, NewOnCallParticipant, NewOnCallSchedule, OrgId,
    RotationType, UserId,
};
use uptimepage::storage::{
    ContactStore, OnCallStore, PgContactStore, PgOnCallStore, create_org_with_owner,
};
use uuid::Uuid;

/// Create an org and return it plus its owner (already a member).
async fn seed_org(pool: &PgPool, prefix: &str) -> (OrgId, UserId) {
    let user = make_user(pool, prefix).await;
    let org = create_org_with_owner(pool, user, &unique_slug(prefix), "n")
        .await
        .expect("create org")
        .expect("org created")
        .id;
    (org, user)
}

async fn add_member(pool: &PgPool, org: OrgId, prefix: &str) -> UserId {
    let user = make_user(pool, prefix).await;
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'member')")
        .bind(user.0)
        .bind(org.0)
        .execute(pool)
        .await
        .expect("add member");
    user
}

async fn seed_channel(pool: &PgPool, org: OrgId, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO notification_channels (org_id, name, kind, config, enabled) \
         VALUES ($1, $2, 'webhook', '{\"webhook\":{\"url\":\"http://x/\",\"headers\":{}}}'::jsonb, true) \
         RETURNING id",
    )
    .bind(org.0)
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert channel")
}

fn schedule(name: &str, tz: &str, participants: Vec<UserId>) -> NewOnCallSchedule {
    NewOnCallSchedule {
        name: name.into(),
        timezone: tz.into(),
        layers: vec![NewOnCallLayer {
            name: Some("primary".into()),
            rotation_type: RotationType::Daily,
            rotation_length_secs: 86_400,
            handoff_at: "2026-06-01T00:00:00Z".parse().unwrap(),
            layer_order: 0,
            participants: participants
                .into_iter()
                .map(|u| NewOnCallParticipant { user_id: u })
                .collect(),
        }],
    }
}

#[tokio::test]
#[ignore]
async fn create_get_replace_delete_roundtrip_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, owner) = seed_org(&pool, "ocroundtrip").await;
    let second = add_member(&pool, org, "ocmember").await;
    let store = PgOnCallStore::new(pool.clone());

    let created = store
        .create(org, schedule("primary", "UTC", vec![owner, second]), 10)
        .await
        .expect("create");
    assert_eq!(created.layers.len(), 1);
    assert_eq!(created.layers[0].participants.len(), 2);
    // Position is the input order.
    assert_eq!(created.layers[0].participants[0].user_id, owner);

    let fetched = store.get(org, created.schedule.id).await.unwrap().unwrap();
    assert_eq!(fetched.layers[0].participants.len(), 2);
    assert_eq!(store.list(org).await.unwrap().len(), 1);

    // Replace with a single-participant layer.
    let replaced = store
        .replace(
            org,
            created.schedule.id,
            schedule("primary", "UTC", vec![owner]),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replaced.layers[0].participants.len(), 1);

    assert!(store.delete(org, created.schedule.id).await.unwrap());
    assert!(store.get(org, created.schedule.id).await.unwrap().is_none());
    assert!(store.list(org).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn rejects_non_member_participant_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, _owner) = seed_org(&pool, "ocidor").await;
    let (_other_org, outsider) = seed_org(&pool, "ocidoroth").await;
    let store = PgOnCallStore::new(pool.clone());

    let err = store
        .create(org, schedule("x", "UTC", vec![outsider]), 10)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        uptimepage::error::AppError::Unprocessable { .. }
    ));
    // The failed insert rolled back — no orphan schedule.
    assert!(store.list(org).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn override_crud_and_resolution_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, owner) = seed_org(&pool, "ocover").await;
    let cover = add_member(&pool, org, "occover").await;
    let store = PgOnCallStore::new(pool.clone());
    let sched = store
        .create(org, schedule("primary", "UTC", vec![owner]), 10)
        .await
        .unwrap();

    let at = "2026-06-01T12:00:00Z".parse().unwrap();
    assert_eq!(
        store.resolve_now(org, sched.schedule.id, at).await.unwrap(),
        vec![owner]
    );

    let ov = store
        .add_override(
            org,
            sched.schedule.id,
            Some(owner),
            NewOnCallOverride {
                user_id: cover,
                starts_at: "2026-06-01T00:00:00Z".parse().unwrap(),
                ends_at: "2026-06-02T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.resolve_now(org, sched.schedule.id, at).await.unwrap(),
        vec![cover]
    );

    assert!(
        store
            .delete_override(org, sched.schedule.id, ov.id)
            .await
            .unwrap()
    );
    assert_eq!(
        store.resolve_now(org, sched.schedule.id, at).await.unwrap(),
        vec![owner]
    );
}

#[tokio::test]
#[ignore]
async fn schedule_target_fk_and_contact_resolution_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    use uptimepage::domain::{
        EscalationTargetType, NewEscalationPolicy, NewEscalationStep, NewEscalationTarget,
    };
    use uptimepage::storage::{EscalationPolicyStore, PgEscalationPolicyStore};

    let (org, owner) = seed_org(&pool, "ocfk").await;
    let on_call = PgOnCallStore::new(pool.clone());
    let sched = on_call
        .create(org, schedule("primary", "UTC", vec![owner]), 10)
        .await
        .unwrap();

    // A policy step can target the schedule now that 021 wired the FK.
    let esc = PgEscalationPolicyStore::new(pool.clone());
    let policy = esc
        .create(
            org,
            NewEscalationPolicy {
                name: "ladder".into(),
                description: None,
                repeat_count: 0,
                steps: vec![NewEscalationStep {
                    level: 1,
                    delay_secs: 300,
                    targets: vec![NewEscalationTarget {
                        target_type: EscalationTargetType::Schedule,
                        user_id: None,
                        schedule_id: Some(sched.schedule.id),
                        channel_id: None,
                    }],
                }],
            },
            10,
        )
        .await
        .expect("policy with schedule target");
    assert_eq!(
        policy.steps[0].targets[0].schedule_id,
        Some(sched.schedule.id)
    );

    // A foreign schedule id is rejected by the store's IDOR guard.
    let (_other, _) = seed_org(&pool, "ocfkoth").await;
    let bogus = Uuid::now_v7();
    let err = esc
        .create(
            org,
            NewEscalationPolicy {
                name: "bad".into(),
                description: None,
                repeat_count: 0,
                steps: vec![NewEscalationStep {
                    level: 1,
                    delay_secs: 0,
                    targets: vec![NewEscalationTarget {
                        target_type: EscalationTargetType::Schedule,
                        user_id: None,
                        schedule_id: Some(bogus),
                        channel_id: None,
                    }],
                }],
            },
            10,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        uptimepage::error::AppError::Unprocessable { .. }
    ));

    // Contact channels: a member's set, validated against org ownership.
    let contacts = PgContactStore::new(pool.clone());
    let c1 = seed_channel(&pool, org, "alice-tg").await;
    let c2 = seed_channel(&pool, org, "alice-slack").await;
    contacts
        .replace_for_user(org, owner, vec![c1, c2])
        .await
        .unwrap();
    assert_eq!(contacts.for_user(org, owner).await.unwrap().len(), 2);
    contacts
        .replace_for_user(org, owner, vec![c1])
        .await
        .unwrap();
    assert_eq!(contacts.for_user(org, owner).await.unwrap(), vec![c1]);
}

#[tokio::test]
#[ignore]
async fn contact_rejects_cross_org_channel_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, owner) = seed_org(&pool, "occross").await;
    let (other, _) = seed_org(&pool, "occrossoth").await;
    let foreign = seed_channel(&pool, other, "theirs").await;
    let contacts = PgContactStore::new(pool.clone());

    let err = contacts
        .replace_for_user(org, owner, vec![foreign])
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        uptimepage::error::AppError::Unprocessable { .. }
    ));
    // The rollback left no contact rows behind.
    assert!(contacts.for_user(org, owner).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn schedules_are_isolated_per_org_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (a, owner_a) = seed_org(&pool, "ocisoa").await;
    let (b, owner_b) = seed_org(&pool, "ocisob").await;
    let store = PgOnCallStore::new(pool.clone());

    let sa = store
        .create(a, schedule("shared", "UTC", vec![owner_a]), 10)
        .await
        .unwrap();
    // Same name in another org is allowed and invisible across the tenant line.
    store
        .create(b, schedule("shared", "UTC", vec![owner_b]), 10)
        .await
        .unwrap();
    assert!(store.get(b, sa.schedule.id).await.unwrap().is_none());
    assert!(!store.delete(b, sa.schedule.id).await.unwrap());
    assert!(store.get(a, sa.schedule.id).await.unwrap().is_some());
}

#[tokio::test]
#[ignore]
async fn create_enforces_quota_and_unique_name_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, owner) = seed_org(&pool, "ocquota").await;
    let store = PgOnCallStore::new(pool.clone());

    store
        .create(org, schedule("a", "UTC", vec![owner]), 1)
        .await
        .unwrap();
    let over = store
        .create(org, schedule("b", "UTC", vec![owner]), 1)
        .await
        .unwrap_err();
    assert!(matches!(
        over,
        uptimepage::error::AppError::Unprocessable { .. }
    ));
    let dup = store
        .create(org, schedule("a", "UTC", vec![owner]), 10)
        .await
        .unwrap_err();
    assert!(matches!(
        dup,
        uptimepage::error::AppError::Unprocessable { .. }
    ));
    // A deleted name frees up for reuse (partial unique index on deleted_at).
    let live = store.list(org).await.unwrap();
    store.delete(org, live[0].id).await.unwrap();
    store
        .create(org, schedule("a", "UTC", vec![owner]), 10)
        .await
        .unwrap();
}
