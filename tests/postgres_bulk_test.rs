//! PG-backed integration tests. Provisioned per-test via `#[sqlx::test]` which
//! requires `DATABASE_URL` to be set. Tests are skipped at the cargo level when
//! the env var is absent (sqlx::test refuses to run without it).

mod common;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use sqlx::PgPool;
use status_monitor::domain::{CheckSpec, ExpectedStatus, NewTarget};
use status_monitor::security::Cipher;
use status_monitor::storage::{PostgresTargetStore, TargetStore};
use url::Url;

use crate::common::default_http_check;

fn test_cipher() -> Arc<Cipher> {
    let kek = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    Arc::new(Cipher::from_base64(&kek).unwrap())
}

fn make(name: &str, tags: Vec<String>) -> NewTarget {
    let url = Url::parse("https://example.com/").unwrap();
    NewTarget {
        name: name.into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags,
        alerts: Default::default(),
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    }
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn bulk_create_with_ragged_tags(pool: PgPool) {
    let store = PostgresTargetStore::from_pool(pool, None);
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
    let store = PostgresTargetStore::from_pool(pool, None);
    let result = store.bulk_create(vec![]).await.expect("empty bulk ok");
    assert!(result.is_empty());
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn credentials_stored_as_ciphertext_envelope(pool: PgPool) {
    let store = PostgresTargetStore::from_pool(pool.clone(), Some(test_cipher()));
    let url = Url::parse("https://example.com/").unwrap();
    let mut http = default_http_check(url, ExpectedStatus::Exact(200));
    http.basic_auth = Some(("alice".into(), "s3cret".into()));
    http.bearer_token = Some("tok.en.value".into());
    let new = NewTarget {
        name: "with-secrets".into(),
        check: CheckSpec::Http(http),
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: Default::default(),
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    };

    let created = store.create(new).await.expect("create");

    // Round-trip via the store decrypts.
    let fetched = store.get(created.id).await.expect("get").expect("present");
    match fetched.check {
        CheckSpec::Http(h) => {
            assert_eq!(h.basic_auth, Some(("alice".into(), "s3cret".into())));
            assert_eq!(h.bearer_token.as_deref(), Some("tok.en.value"));
        }
        _ => panic!("expected http check"),
    }

    // Raw row holds ciphertext envelope, not plaintext.
    let raw: (serde_json::Value,) = sqlx::query_as("SELECT check_spec FROM targets WHERE id = $1")
        .bind(created.id)
        .fetch_one(&pool)
        .await
        .expect("raw select");
    let raw_str = raw.0.to_string();
    assert!(!raw_str.contains("alice"), "plaintext leaked: {raw_str}");
    assert!(!raw_str.contains("s3cret"), "plaintext leaked: {raw_str}");
    assert!(
        !raw_str.contains("tok.en.value"),
        "plaintext leaked: {raw_str}"
    );
    assert!(
        raw_str.contains("\"$enc\":\"v1:"),
        "envelope missing: {raw_str}"
    );
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn no_credentials_no_envelope(pool: PgPool) {
    let store = PostgresTargetStore::from_pool(pool.clone(), Some(test_cipher()));
    let new = make("plain", vec![]);
    let created = store.create(new).await.expect("create");

    let raw: (serde_json::Value,) = sqlx::query_as("SELECT check_spec FROM targets WHERE id = $1")
        .bind(created.id)
        .fetch_one(&pool)
        .await
        .expect("raw select");
    let raw_str = raw.0.to_string();
    assert!(!raw_str.contains("$enc"), "unexpected envelope: {raw_str}");
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn legacy_plaintext_row_decrypts_passthrough(pool: PgPool) {
    // Simulate a row written before encryption was enabled: insert plaintext
    // basic_auth directly, then read via a cipher-enabled store.
    let check_json = serde_json::json!({
        "type": "http",
        "url": "https://example.com/",
        "method": "GET",
        "timeout": 3000,
        "follow_redirects": false,
        "max_redirects": 0,
        "expected_status": { "kind": "exact", "value": 200 },
        "expected_body_contains": null,
        "headers": {},
        "body": null,
        "verify_tls": true,
        "basic_auth": ["legacy", "old-pass"],
        "bearer_token": null
    });
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO targets (id, name, check_spec, interval_secs, enabled, tags) \
         VALUES ($1, 'legacy', $2, 30, true, ARRAY[]::text[])",
    )
    .bind(id)
    .bind(check_json)
    .execute(&pool)
    .await
    .expect("insert legacy row");

    let store = PostgresTargetStore::from_pool(pool, Some(test_cipher()));
    let fetched = store.get(id).await.expect("get").expect("present");
    match fetched.check {
        CheckSpec::Http(h) => {
            assert_eq!(h.basic_auth, Some(("legacy".into(), "old-pass".into())));
        }
        _ => panic!("expected http check"),
    }
}
