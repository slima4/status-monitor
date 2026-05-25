//! PG-backed integration tests. Provisioned per-test via `#[sqlx::test]` which
//! requires `DATABASE_URL` to be set. Tests are skipped at the cargo level when
//! the env var is absent (sqlx::test refuses to run without it).

mod common;

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use status_monitor::domain::{CheckSpec, ExpectedStatus, NewTarget, TargetUpdate};
use status_monitor::security::Cipher;
use status_monitor::storage::{PostgresTargetStore, TargetStore};
use url::Url;
use uuid::Uuid;

use crate::common::{default_http_check, test_cipher};

/// Provision a fresh org with a unique slug and return a store paired with
/// the new org's id. Each test gets its own org so parallel test runs don't
/// step on each other's `targets` rows.
async fn store_with_default_org(
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
) -> (PostgresTargetStore, status_monitor::domain::OrgId) {
    let slug = format!("bulk-{}", Uuid::now_v7().simple());
    let slug = &slug[..slug.len().min(30)];
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ($1, 'Bulk Test') RETURNING id",
    )
    .bind(slug)
    .fetch_one(&pool)
    .await
    .expect("insert bulk-test org");
    let org_id = status_monitor::domain::OrgId(id);
    let store = PostgresTargetStore::from_pool(pool, cipher);
    (store, org_id)
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
        group_name: None,
        owner_user_id: None,
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
    let (store, org) = store_with_default_org(pool, None).await;
    let items = vec![
        make("t1", vec!["a".into(), "b".into()]),
        make("t2", vec![]),
        make("t3", vec!["only".into()]),
    ];

    let created = store
        .bulk_create(org, items, i64::MAX)
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
    let (store, org) = store_with_default_org(pool, None).await;
    let result = store
        .bulk_create(org, vec![], i64::MAX)
        .await
        .expect("empty bulk ok");
    assert!(result.is_empty());
}

/// PATCH must distinguish "field omitted → keep" from "field present as
/// JSON null → clear". The status-page curation UI relies on the clear
/// path to revert a component to its real monitor name. Exercises the
/// double-Option + CASE-WHEN UPDATE bind renumbering directly.
#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn target_update_public_overlay_set_keep_clear(pool: PgPool) {
    let (store, org) = store_with_default_org(pool, None).await;
    let t = store
        .create(org, make("svc", vec![]), i64::MAX)
        .await
        .expect("create");

    // Set the overlay.
    let r = store
        .update(
            org,
            t.id,
            TargetUpdate {
                public_name: Some(Some("Public API".into())),
                public_group: Some(Some("Core".into())),
                ..Default::default()
            },
        )
        .await
        .expect("update ok")
        .expect("target exists");
    assert_eq!(r.public_name.as_deref(), Some("Public API"));
    assert_eq!(r.public_group.as_deref(), Some("Core"));

    // Omit the overlay fields (change something else) → must stay unchanged.
    let r = store
        .update(
            org,
            t.id,
            TargetUpdate {
                public_status: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("update ok")
        .expect("target exists");
    assert!(r.public_status);
    assert_eq!(
        r.public_name.as_deref(),
        Some("Public API"),
        "omitted field must be left unchanged"
    );
    assert_eq!(r.public_group.as_deref(), Some("Core"));

    // Explicit null → clear back to no override.
    let r = store
        .update(
            org,
            t.id,
            TargetUpdate {
                public_name: Some(None),
                public_group: Some(None),
                ..Default::default()
            },
        )
        .await
        .expect("update ok")
        .expect("target exists");
    assert_eq!(r.public_name, None, "explicit null must clear");
    assert_eq!(r.public_group, None, "explicit null must clear");
}

#[sqlx::test(migrations = "./migrations/postgres")]
#[ignore = "requires DATABASE_URL — run via DATABASE_URL=... cargo test -- --ignored"]
async fn credentials_stored_as_ciphertext_envelope(pool: PgPool) {
    let (store, org) = store_with_default_org(pool.clone(), Some(test_cipher())).await;
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
        group_name: None,
        owner_user_id: None,
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    };

    let created = store.create(org, new, i64::MAX).await.expect("create");

    // Round-trip via the store decrypts.
    let fetched = store
        .get(org, created.id)
        .await
        .expect("get")
        .expect("present");
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
    let (store, org) = store_with_default_org(pool.clone(), Some(test_cipher())).await;
    let new = make("plain", vec![]);
    let created = store.create(org, new, i64::MAX).await.expect("create");

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
    let slug = format!("bulk-{}", Uuid::now_v7().simple());
    let slug = &slug[..slug.len().min(30)];
    let (org_uuid,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ($1, 'Bulk Test') RETURNING id",
    )
    .bind(slug)
    .fetch_one(&pool)
    .await
    .expect("insert bulk-test org");
    let org_id = status_monitor::domain::OrgId(org_uuid);
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO targets (id, org_id, name, check_spec, interval_secs, enabled, tags) \
         VALUES ($1, $2, 'legacy', $3, 30, true, ARRAY[]::text[])",
    )
    .bind(id)
    .bind(org_id.0)
    .bind(check_json)
    .execute(&pool)
    .await
    .expect("insert legacy row");

    let store = PostgresTargetStore::from_pool(pool, Some(test_cipher()));
    let fetched = store.get(org_id, id).await.expect("get").expect("present");
    match fetched.check {
        CheckSpec::Http(h) => {
            assert_eq!(h.basic_auth, Some(("legacy".into(), "old-pass".into())));
        }
        _ => panic!("expected http check"),
    }
}
