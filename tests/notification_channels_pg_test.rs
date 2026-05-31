//! Live-Postgres contract for `notification_channels`: cross-tenant isolation
//! on every store method, the per-org quota cap, secrets sealed at rest by the
//! credentials KEK, and the org scoping of the channel-id lookup that the
//! target→channel binding IDOR guard (`verify_alert_channels`) is built on.
//! The in-memory suite (`notification_channels_test`) covers the
//! HTTP/redaction contract; this one exercises the real SQL.
//!
//! Live-PG ignored: needs `DATABASE_URL`. Migrations are auto-applied by
//! `pg_pool_from_env` on first connect — point it at a throwaway DB to also
//! validate the migrations themselves.

mod common;

use std::time::Duration;

use uptimepage::api::error::codes;
use uptimepage::domain::{
    AlertBinding, ChannelConfig, CheckSpec, ExpectedStatus, NewNotificationChannel, NewTarget,
    NotificationChannelUpdate, TargetAlerts, WriteSource,
};
use uptimepage::error::AppError;
use uptimepage::storage::{
    NotificationChannelStore, PgNotificationChannelStore, PostgresTargetStore, TargetStore,
    create_org_with_owner,
};

use common::{default_http_check, make_user, pg_pool_from_env, test_cipher, unique_slug};

fn slack(name: &str, secret: &str) -> NewNotificationChannel {
    NewNotificationChannel {
        name: name.into(),
        config: ChannelConfig::Slack {
            webhook_url: format!("https://hooks.slack.com/services/{secret}"),
        },
        enabled: true,
    }
}

/// Two owner-orgs (one user each). Returns `(org_a, org_b, user_a, user_b)`.
async fn two_orgs(
    pool: &sqlx::PgPool,
    tag: &str,
) -> (
    uptimepage::domain::OrgId,
    uptimepage::domain::OrgId,
    uptimepage::domain::UserId,
    uptimepage::domain::UserId,
) {
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

async fn cleanup(
    pool: &sqlx::PgPool,
    orgs: &[uptimepage::domain::OrgId],
    users: &[uptimepage::domain::UserId],
) {
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
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn channels_isolated_across_orgs_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "nc-iso").await;
    let store = PgNotificationChannelStore::new(pool.clone(), None);

    let ch = store
        .create(org_a, slack("A Ops", "T/B/aaa"), WriteSource::Ui, i64::MAX)
        .await
        .expect("A creates its channel");

    // B cannot see A's channel on any read path …
    assert!(store.get(org_b, ch.id).await.unwrap().is_none());
    assert!(store.list(org_b).await.unwrap().is_empty());
    assert!(
        store
            .existing_channel_ids(org_b, &[ch.id])
            .await
            .unwrap()
            .is_empty()
    );
    // … nor mutate it: update is a no-op miss, delete reports nothing removed.
    assert!(
        store
            .update(
                org_b,
                ch.id,
                NotificationChannelUpdate::default(),
                WriteSource::Ui
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.delete(org_b, ch.id).await.unwrap());

    // A still owns it after B's failed attempts.
    let a_view = store.get(org_a, ch.id).await.unwrap().expect("A sees it");
    assert_eq!(a_view.name, "A Ops");
    assert_eq!(store.list(org_a).await.unwrap().len(), 1);
    assert_eq!(
        store.existing_channel_ids(org_a, &[ch.id]).await.unwrap(),
        vec![ch.id]
    );
    assert!(store.delete(org_a, ch.id).await.unwrap());

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn quota_cap_is_per_org_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "nc-cap").await;
    let store = PgNotificationChannelStore::new(pool.clone(), None);
    let cap = 2;
    // Single-threaded: asserts the rejection contract + per-org scoping + live
    // counting. The advisory-lock concurrency that makes the count+INSERT
    // race-safe is covered by the storage-locks unit suite, not here.

    let a1 = store
        .create(org_a, slack("A1", "T/B/a1"), WriteSource::Ui, cap)
        .await
        .expect("A #1");
    store
        .create(org_a, slack("A2", "T/B/a2"), WriteSource::Ui, cap)
        .await
        .expect("A #2");
    // A is at its cap: the next create is rejected.
    match store
        .create(org_a, slack("A3", "T/B/a3"), WriteSource::Ui, cap)
        .await
    {
        Err(AppError::Unprocessable { code, .. }) => {
            assert_eq!(code, codes::CHANNEL_QUOTA_EXCEEDED)
        }
        other => panic!("expected {}, got {other:?}", codes::CHANNEL_QUOTA_EXCEEDED),
    }

    // The cap is per-org: B is unaffected by A's count.
    store
        .create(org_b, slack("B1", "T/B/b1"), WriteSource::Ui, cap)
        .await
        .expect("B #1 (A's count must not affect B)");
    store
        .create(org_b, slack("B2", "T/B/b2"), WriteSource::Ui, cap)
        .await
        .expect("B #2");

    // Cap tracks the live count: freeing a slot lets A create again.
    assert!(store.delete(org_a, a1.id).await.unwrap());
    store
        .create(org_a, slack("A4", "T/B/a4"), WriteSource::Ui, cap)
        .await
        .expect("A can create again after deleting one");

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn channel_config_sealed_at_rest_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, _org_b, user_a, user_b) = two_orgs(&pool, "nc-seal").await;
    let store = PgNotificationChannelStore::new(pool.clone(), Some(test_cipher()));

    let secret = "T/B/zzSEKRETzz";
    let ch = store
        .create(org_a, slack("Sealed", secret), WriteSource::Ui, i64::MAX)
        .await
        .expect("create sealed channel");

    // Raw column is the {"$enc":"v1:…"} envelope — never plaintext.
    let (raw,): (String,) =
        sqlx::query_as("SELECT config::text FROM notification_channels WHERE id = $1")
            .bind(ch.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(raw.contains("$enc"), "config must be sealed: {raw}");
    assert!(
        !raw.contains("zzSEKRETzz"),
        "plaintext secret leaked to disk: {raw}"
    );

    // Opened back through the store the caller sees plaintext again.
    let opened = store.get(org_a, ch.id).await.unwrap().unwrap();
    match opened.config {
        ChannelConfig::Slack { webhook_url } => {
            assert!(webhook_url.ends_with(secret))
        }
        other => panic!("unexpected variant: {other:?}"),
    }

    cleanup(&pool, &[org_a], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn target_alert_binding_channel_lookup_is_org_scoped_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "nc-bind").await;
    let channels = PgNotificationChannelStore::new(pool.clone(), None);
    let targets = PostgresTargetStore::from_pool(pool.clone(), None);

    let ch = channels
        .create(org_a, slack("Bound", "T/B/bind"), WriteSource::Ui, i64::MAX)
        .await
        .expect("A's channel");

    let url = url::Url::parse("https://example.com/").unwrap();
    let new_target = NewTarget {
        name: "alerting".into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: TargetAlerts(vec![AlertBinding {
            channel_id: ch.id,
            after_failures: 3,
            notify_recovery: true,
        }]),
        group_name: None,
        owner_user_id: None,
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    };
    let created = targets
        .create(org_a, new_target, WriteSource::Ui, i64::MAX)
        .await
        .expect("A creates a target bound to its own channel");

    // Binding round-trips.
    let fetched = targets.get(org_a, created.id).await.unwrap().unwrap();
    let bound: Vec<_> = fetched.alerts.iter().map(|b| b.channel_id).collect();
    assert_eq!(bound, vec![ch.id]);

    // The IDOR guard: the channel id is "existing" only for its owning org,
    // so a foreign target can never resolve another tenant's channel.
    assert_eq!(
        channels
            .existing_channel_ids(org_a, &[ch.id])
            .await
            .unwrap(),
        vec![ch.id]
    );
    assert!(
        channels
            .existing_channel_ids(org_b, &[ch.id])
            .await
            .unwrap()
            .is_empty()
    );

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}
