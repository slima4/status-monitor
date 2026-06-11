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

use chrono::Utc;
use uptimepage::api::error::codes;
use uptimepage::domain::{
    AlertBinding, ChannelConfig, CheckSpec, ExpectedStatus, NewIncidentNotification,
    NewNotificationChannel, NewTarget, NotificationChannelUpdate, NotificationReason,
    NotificationStatus, SlackConfig, TargetAlerts, WriteSource,
};
use uptimepage::error::AppError;
use uptimepage::storage::{
    Actor, IncidentOpsStore, NotificationChannelStore, PgIncidentOpsStore,
    PgNotificationChannelStore, PostgresTargetStore, TargetStore, create_org_with_owner,
};

use common::{default_http_check, make_user, pg_pool_from_env, test_cipher, unique_slug};

fn slack(name: &str, secret: &str) -> NewNotificationChannel {
    NewNotificationChannel {
        name: name.into(),
        config: ChannelConfig::Slack(SlackConfig {
            webhook_url: format!("https://hooks.slack.com/services/{secret}"),
        }),
        enabled: true,
        external_ref: None,
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
        ChannelConfig::Slack(SlackConfig { webhook_url }) => {
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
        alerts: TargetAlerts(vec![AlertBinding { channel_id: ch.id }]),
        region_policy: Default::default(),
        alert_confirmations: 2,
        notify_recovery: true,
        renotify_interval_secs: 3600,
        group_name: None,
        owner_user_id: None,
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

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn unbind_channel_scrubs_only_that_binding_in_org_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "nc-unbind").await;
    let channels = PgNotificationChannelStore::new(pool.clone(), None);
    let targets = PostgresTargetStore::from_pool(pool.clone(), None);

    let ch_a = channels
        .create(org_a, slack("A", "T/B/ua"), WriteSource::Ui, i64::MAX)
        .await
        .unwrap();
    let ch_b = channels
        .create(org_a, slack("B", "T/B/ub"), WriteSource::Ui, i64::MAX)
        .await
        .unwrap();

    let mk_target = |name: &str, alerts: Vec<AlertBinding>| {
        let url = url::Url::parse("https://example.com/").unwrap();
        NewTarget {
            name: name.into(),
            check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
            interval: Duration::from_secs(30),
            enabled: true,
            tags: vec![],
            alerts: TargetAlerts(alerts),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
        }
    };
    let both = targets
        .create(
            org_a,
            mk_target(
                "both",
                vec![
                    AlertBinding {
                        channel_id: ch_a.id,
                    },
                    AlertBinding {
                        channel_id: ch_b.id,
                    },
                ],
            ),
            WriteSource::Ui,
            i64::MAX,
        )
        .await
        .unwrap();
    let only_a = targets
        .create(
            org_a,
            mk_target(
                "only-a",
                vec![AlertBinding {
                    channel_id: ch_a.id,
                }],
            ),
            WriteSource::Ui,
            i64::MAX,
        )
        .await
        .unwrap();
    // Store-level write: another org carrying the same channel id must be
    // untouched by org_a's scrub (handler validation would never allow this
    // binding, which is exactly why the SQL needs its own org guard).
    let foreign = targets
        .create(
            org_b,
            mk_target(
                "foreign",
                vec![AlertBinding {
                    channel_id: ch_a.id,
                }],
            ),
            WriteSource::Ui,
            i64::MAX,
        )
        .await
        .unwrap();

    let touched = targets.unbind_channel(org_a, ch_a.id).await.unwrap();
    assert_eq!(touched, 2);

    let both = targets.get(org_a, both.id).await.unwrap().unwrap();
    let bound: Vec<_> = both.alerts.iter().map(|b| b.channel_id).collect();
    assert_eq!(bound, vec![ch_b.id], "sibling binding must survive");

    let only_a = targets.get(org_a, only_a.id).await.unwrap().unwrap();
    assert!(only_a.alerts.is_empty(), "binding must be scrubbed");

    let foreign = targets.get(org_b, foreign.id).await.unwrap().unwrap();
    assert_eq!(
        foreign.alerts.iter().count(),
        1,
        "foreign org's bindings must be untouched"
    );

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn due_for_renotify_selects_overdue_open_unacked_live_pg() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "renotify").await;
    let org = create_org_with_owner(&pool, user, &unique_slug("renotify"), "R", 3)
        .await
        .unwrap()
        .expect("org")
        .id;
    let targets = PostgresTargetStore::from_pool(pool.clone(), None);
    let ops = PgIncidentOpsStore::new(pool.clone());

    let mk_target = |name: &str, renotify: u32| {
        let url = url::Url::parse("https://example.com/").unwrap();
        NewTarget {
            name: name.into(),
            check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
            interval: Duration::from_secs(30),
            enabled: true,
            tags: vec![],
            alerts: TargetAlerts::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: renotify,
            group_name: None,
            owner_user_id: None,
        }
    };

    // Open a triggered incident for `target_id` and record one successful page
    // `page_age` ago. Returns the incident id.
    async fn open_paged_incident(
        pool: &sqlx::PgPool,
        ops: &PgIncidentOpsStore,
        org: uptimepage::domain::OrgId,
        target_id: uuid::Uuid,
        page_age: chrono::Duration,
    ) -> uuid::Uuid {
        let (inc,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO incidents \
                (org_id, target_id, started_at, status_at_start, state, severity, urgency, origin, visibility) \
             VALUES ($1, $2, now() - interval '3 hours', 'down', 'triggered', 'major', 'high', 'monitor', 'internal') \
             RETURNING id",
        )
        .bind(org.0)
        .bind(target_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let notif = ops
            .record_notification(NewIncidentNotification {
                org,
                incident_id: inc,
                escalation_level: Some(0),
                target_user_id: None,
                channel_id: None,
                transport: "slack".into(),
                reason: NotificationReason::Opened,
                status: NotificationStatus::Sent,
                attempt: 1,
                error: None,
                sent_at: Some(Utc::now() - page_age),
            })
            .await
            .unwrap();
        // record_notification stamps created_at = now(); backdate it so the
        // reminder cadence (which keys off the last attempt's created_at) sees
        // this page as `page_age` old.
        sqlx::query("UPDATE incident_notifications SET created_at = $2 WHERE id = $1")
            .bind(notif)
            .bind(Utc::now() - page_age)
            .execute(pool)
            .await
            .unwrap();
        inc
    }

    let overdue_t = targets
        .create(org, mk_target("overdue", 3600), WriteSource::Ui, i64::MAX)
        .await
        .unwrap();
    let recent_t = targets
        .create(org, mk_target("recent", 3600), WriteSource::Ui, i64::MAX)
        .await
        .unwrap();
    let off_t = targets
        .create(org, mk_target("off", 0), WriteSource::Ui, i64::MAX)
        .await
        .unwrap();

    let overdue =
        open_paged_incident(&pool, &ops, org, overdue_t.id, chrono::Duration::hours(2)).await;
    let recent =
        open_paged_incident(&pool, &ops, org, recent_t.id, chrono::Duration::minutes(1)).await;
    let off = open_paged_incident(&pool, &ops, org, off_t.id, chrono::Duration::hours(2)).await;

    let due: Vec<uuid::Uuid> = ops
        .due_for_renotify(Utc::now(), 100)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert!(due.contains(&overdue), "an overdue open incident is due");
    assert!(
        !due.contains(&recent),
        "a recently-paged incident is below its interval"
    );
    assert!(
        !due.contains(&off),
        "renotify_interval_secs = 0 disables reminders"
    );

    // Acknowledging the overdue one silences it.
    ops.acknowledge(org, overdue, Actor::System, None)
        .await
        .unwrap();
    let due: Vec<uuid::Uuid> = ops
        .due_for_renotify(Utc::now(), 100)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert!(
        !due.contains(&overdue),
        "an acknowledged incident is not reminded"
    );

    cleanup(&pool, &[org], &[user]).await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn telegram_lifecycle_disable_by_external_ref() {
    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b, user_a, user_b) = two_orgs(&pool, "tg-life").await;
    let store = PgNotificationChannelStore::new(pool.clone(), None);

    let chat = format!("-100{}", uuid::Uuid::now_v7().simple());
    let linked = |name: &str, chat: &str| uptimepage::domain::NewNotificationChannel {
        name: name.into(),
        config: uptimepage::domain::ChannelConfig::TelegramApp(
            uptimepage::domain::TelegramAppConfig {
                chat_id: chat.into(),
                chat_title: Some("Ops".into()),
            },
        ),
        enabled: true,
        external_ref: Some(chat.into()),
    };
    // Two orgs share the kicked chat; org A has a second, unrelated link.
    let a = store
        .create(org_a, linked("prod", &chat), WriteSource::Ui, 10)
        .await
        .unwrap();
    store
        .create(org_b, linked("ops", &chat), WriteSource::Ui, 10)
        .await
        .unwrap();
    let other_chat = format!("-200{}", uuid::Uuid::now_v7().simple());
    store
        .create(org_a, linked("other", &other_chat), WriteSource::Ui, 10)
        .await
        .unwrap();

    let kind = uptimepage::domain::ChannelKind::TelegramApp;
    assert_eq!(store.count_by_external_ref(kind, &chat).await.unwrap(), 2);
    assert_eq!(
        store
            .disable_by_external_ref(kind, &chat, "unlinked from the Telegram side")
            .await
            .unwrap(),
        2
    );
    // Idempotent on already-disabled rows.
    assert_eq!(
        store
            .disable_by_external_ref(kind, &chat, "unlinked from the Telegram side")
            .await
            .unwrap(),
        0
    );
    let got = store.get(org_a, a.id).await.unwrap().unwrap();
    assert!(!got.enabled);
    assert_eq!(
        got.disabled_reason.as_deref(),
        Some("unlinked from the Telegram side")
    );
    // The unrelated chat is untouched and still counted by its own ref.
    assert_eq!(
        store
            .count_by_external_ref(kind, &other_chat)
            .await
            .unwrap(),
        1
    );

    // A name-only PATCH while disabled keeps the note…
    let renamed = store
        .update(
            org_a,
            a.id,
            NotificationChannelUpdate {
                name: Some("prod-tg".into()),
                ..Default::default()
            },
            WriteSource::Ui,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!renamed.enabled);
    assert!(renamed.disabled_reason.is_some());

    // …and re-enabling clears it.
    let re = store
        .update(
            org_a,
            a.id,
            NotificationChannelUpdate {
                enabled: Some(true),
                ..Default::default()
            },
            WriteSource::Ui,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(re.enabled && re.disabled_reason.is_none());

    cleanup(&pool, &[org_a, org_b], &[user_a, user_b]).await;
}
