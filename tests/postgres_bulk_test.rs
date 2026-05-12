//! PG-backed integration tests. Provisioned per-test via `#[sqlx::test]` which
//! requires `DATABASE_URL` to be set. Tests are skipped at the cargo level when
//! the env var is absent (sqlx::test refuses to run without it).

mod common;

use std::time::Duration;

use sqlx::PgPool;
use status_monitor::domain::{CheckSpec, ExpectedStatus, NewTarget};
use status_monitor::storage::{PostgresTargetStore, TargetStore};
use url::Url;

use crate::common::default_http_check;

fn make(name: &str, tags: Vec<String>) -> NewTarget {
    let url = Url::parse("https://example.com/").unwrap();
    NewTarget {
        name: name.into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags,
    }
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn bulk_create_with_ragged_tags(pool: PgPool) {
    let store = PostgresTargetStore::from_pool(pool);
    let items = vec![
        make("t1", vec!["a".into(), "b".into()]),
        make("t2", vec![]),
        make("t3", vec!["only".into()]),
    ];

    let created = store
        .bulk_create(items)
        .await
        .expect("bulk_create succeeds");

    assert_eq!(created.len(), 3);
    assert_eq!(created[0].name, "t1");
    assert_eq!(created[0].tags, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(created[1].name, "t2");
    assert!(created[1].tags.is_empty());
    assert_eq!(created[2].name, "t3");
    assert_eq!(created[2].tags, vec!["only".to_string()]);
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn bulk_create_empty_is_noop(pool: PgPool) {
    let store = PostgresTargetStore::from_pool(pool);
    let result = store.bulk_create(vec![]).await.expect("empty bulk ok");
    assert!(result.is_empty());
}
