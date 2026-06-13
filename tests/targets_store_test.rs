//! Live-PG coverage for the targets `kind` filter + `count_by_kind` pushdown:
//! the generated `kind` column drives the SQL filter, and chip tallies are
//! org-wide (not page-scoped).

mod common;

use std::time::Duration;

use uptimepage::domain::{CheckSpec, ExpectedStatus, NewTarget, OrgId, TcpCheck, WriteSource};
use uptimepage::storage::{PostgresTargetStore, TargetFilter, TargetStore, create_org_with_owner};
use url::Url;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn seed(
    store: &PostgresTargetStore,
    org: OrgId,
    name: &str,
    check: CheckSpec,
    group: Option<&str>,
) -> Uuid {
    let nt = NewTarget {
        name: name.into(),
        check,
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: Default::default(),
        region_policy: Default::default(),
        alert_confirmations: 2,
        notify_recovery: true,
        renotify_interval_secs: 3600,
        group_name: group.map(str::to_owned),
        owner_user_id: None,
    };
    store
        .create(org, nt, WriteSource::Ui, i64::MAX)
        .await
        .unwrap()
        .id
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn kind_filter_and_counts_are_org_wide() {
    let Some((db, name)) = common::fresh_test_db("targets_kind").await else {
        return;
    };
    let pool = common::open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = common::make_user(&pool, "k").await;
    let org = create_org_with_owner(&pool, user, &common::unique_slug("k"), "Co", 10)
        .await
        .unwrap()
        .unwrap();
    let store = PostgresTargetStore::from_pool(pool.clone(), None);

    let http = || {
        CheckSpec::Http(common::default_http_check(
            Url::parse("https://example.com/").unwrap(),
            ExpectedStatus::Exact(200),
        ))
    };
    let tcp = || {
        CheckSpec::Tcp(TcpCheck {
            host: "db.example.com".into(),
            port: 5432,
            timeout: Duration::from_secs(3),
        })
    };
    seed(&store, org.id, "a", http(), None).await;
    seed(&store, org.id, "b", http(), None).await;
    seed(&store, org.id, "c", tcp(), None).await;

    // Org-wide tally keyed by the check_spec type tag.
    let counts = store
        .count_by_kind(org.id, TargetFilter::default())
        .await
        .unwrap();
    assert_eq!(counts.get("http").copied(), Some(2));
    assert_eq!(counts.get("tcp").copied(), Some(1));

    // The generated `kind` column drives the SQL filter.
    let only_tcp = store
        .list(
            org.id,
            TargetFilter {
                kind: Some("tcp".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(only_tcp.len(), 1);
    assert_eq!(only_tcp[0].name, "c");

    let only_http = store
        .list(
            org.id,
            TargetFilter {
                kind: Some("http".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(only_http.len(), 2);

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn region_filter_and_distinct_groups_are_org_wide() {
    let Some((db, name)) = common::fresh_test_db("targets_region").await else {
        return;
    };
    let pool = common::open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = common::make_user(&pool, "r").await;
    let org = create_org_with_owner(&pool, user, &common::unique_slug("r"), "Co", 10)
        .await
        .unwrap()
        .unwrap();
    let store = PostgresTargetStore::from_pool(pool.clone(), None);

    sqlx::query(
        "INSERT INTO regions (id, name) VALUES ('eu-west','eu-west'),('us-east','us-east')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let http = || {
        CheckSpec::Http(common::default_http_check(
            Url::parse("https://example.com/").unwrap(),
            ExpectedStatus::Exact(200),
        ))
    };
    let a = seed(&store, org.id, "a", http(), Some("alpha")).await;
    let b = seed(&store, org.id, "b", http(), Some("beta")).await;
    seed(&store, org.id, "c", http(), None).await;
    store
        .set_target_regions(org.id, a, &["eu-west".into()])
        .await
        .unwrap();
    store
        .set_target_regions(org.id, b, &["us-east".into()])
        .await
        .unwrap();

    // Region filter lists only targets that run there.
    let eu = store
        .list(
            org.id,
            TargetFilter {
                region: Some("eu-west".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(eu.len(), 1);
    assert_eq!(eu[0].name, "a");

    // Group options are org-wide, sorted, deduped — not page-scoped.
    let groups = store.distinct_groups(org.id).await.unwrap();
    assert_eq!(groups, vec!["alpha".to_string(), "beta".to_string()]);

    common::drop_test_db(&name).await;
}
