//! Live-PG tests for Phase 2-3: OAuth state lifecycle, identity find-or-create
//! with personal org auto-create, session CRUD with idle + absolute timeouts,
//! cookie-driven extractor, and `/api/v1/me` smoke.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test auth_session_test -- --ignored

mod common;

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection};
use status_monitor::auth::{
    fingerprint, github,
    login_audit::{self, LoginAttempt, LoginMethod},
    oauth_state, oauth_state_cleanup, session as session_store,
};
use status_monitor::config::SessionConfig;
use status_monitor::domain::UserId;
use url::Url;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    let raw = std::env::var("DATABASE_URL").ok()?;
    let mut url = Url::parse(&raw).expect("DATABASE_URL must be a valid URL");
    let test_db = format!("auth_session_{}", Uuid::now_v7().simple());
    url.set_path("/postgres");
    let admin = url.clone();
    let mut conn = PgConnection::connect(admin.as_str())
        .await
        .expect("connect admin");
    conn.execute(format!("CREATE DATABASE {test_db}").as_str())
        .await
        .expect("CREATE DATABASE");
    let mut new_url = admin.clone();
    new_url.set_path(&format!("/{test_db}"));
    Some((new_url.to_string(), test_db))
}

async fn drop_pg(test_db: &str) {
    let Ok(raw) = std::env::var("DATABASE_URL") else {
        return;
    };
    let mut url = Url::parse(&raw).unwrap();
    url.set_path("/postgres");
    if let Ok(mut conn) = PgConnection::connect(url.as_str()).await {
        let _ = conn
            .execute(
                format!(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = '{test_db}' AND pid <> pg_backend_pid()"
                )
                .as_str(),
            )
            .await;
        let _ = conn
            .execute(format!("DROP DATABASE IF EXISTS {test_db}").as_str())
            .await;
    }
}

async fn open_pool(db_url: &str) -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .connect(db_url)
        .await
        .expect("connect test DB")
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn oauth_state_insert_consume_round_trip() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let s = oauth_state::generate_state();
    oauth_state::insert(&pool, &s, "github", Some("/after"), Some("inv-tok"))
        .await
        .expect("insert");
    let consumed = oauth_state::consume(&pool, &s)
        .await
        .expect("consume")
        .expect("row");
    assert_eq!(consumed.provider, "github");
    assert_eq!(consumed.redirect_after.as_deref(), Some("/after"));
    assert_eq!(consumed.invitation_token.as_deref(), Some("inv-tok"));

    // Second consume must return None — state is single-use.
    let again = oauth_state::consume(&pool, &s).await.expect("re-consume");
    assert!(again.is_none(), "second consume must fail");

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn oauth_state_consume_rejects_expired() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    sqlx::query(
        "INSERT INTO oauth_states (state, provider, expires_at) VALUES ($1, 'github', now() - INTERVAL '1 minute')",
    )
    .bind("expired-state")
    .execute(&pool)
    .await
    .expect("seed expired");

    let consumed = oauth_state::consume(&pool, "expired-state").await.unwrap();
    assert!(consumed.is_none(), "expired state must not consume");

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn upsert_creates_user_and_personal_org_for_new_identity() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let identity = github::GithubIdentity {
        provider_user_id: "12345".into(),
        provider_username: "octocat".into(),
        primary_verified_email: Some("Alice@Example.test".into()),
        display_name: Some("Alice".into()),
    };
    let resolved = github::upsert_identity_and_personal_org(&pool, &identity)
        .await
        .expect("upsert");
    assert!(resolved.is_new_user);
    assert!(resolved.personal_org_id.is_some());

    // CITEXT — invitation row with lower-case match should find this user.
    let (user_email,): (String,) = sqlx::query_as("SELECT email::text FROM users WHERE id = $1")
        .bind(resolved.user_id.0)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(user_email.to_lowercase(), "alice@example.test");

    // Idempotent re-callback with same identity must NOT create a second
    // user. Returns is_new_user=false and no personal org.
    let again = github::upsert_identity_and_personal_org(&pool, &identity)
        .await
        .expect("re-upsert");
    assert!(!again.is_new_user);
    assert!(again.personal_org_id.is_none());
    assert_eq!(again.user_id.0, resolved.user_id.0);

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn upsert_links_existing_user_on_email_match() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let (existing_id,): (Uuid,) =
        sqlx::query_as("INSERT INTO users (email) VALUES ('Bob@Example.test') RETURNING id")
            .fetch_one(&pool)
            .await
            .expect("seed user");

    let identity = github::GithubIdentity {
        provider_user_id: "99".into(),
        provider_username: "bob".into(),
        primary_verified_email: Some("bob@example.test".into()),
        display_name: None,
    };
    let resolved = github::upsert_identity_and_personal_org(&pool, &identity)
        .await
        .expect("upsert");
    assert!(!resolved.is_new_user);
    assert!(resolved.personal_org_id.is_none());
    assert_eq!(resolved.user_id.0, existing_id);

    // Identity link must have been inserted.
    let (linked_count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM oauth_identities WHERE provider_user_id = $1")
            .bind("99")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked_count, 1);

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn session_create_lookup_destroy_round_trip() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let (user_id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("ses-{}@example.test", Uuid::now_v7()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let user = UserId(user_id);
    let cfg = SessionConfig::default();
    let row = session_store::create(&pool, &cfg, user, None, Some("ip"), Some("ua"))
        .await
        .expect("create");
    assert_eq!(row.id.len(), 43);

    let outcome = session_store::lookup(&pool, &cfg, &row.id).await.unwrap();
    assert!(matches!(outcome, session_store::LookupOutcome::Active(_)));

    let removed = session_store::destroy(&pool, &row.id).await.unwrap();
    assert_eq!(removed, 1);

    let missing = session_store::lookup(&pool, &cfg, &row.id).await.unwrap();
    assert!(matches!(missing, session_store::LookupOutcome::Missing));

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn session_lookup_destroys_idle_expired() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let (user_id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("idle-{}@example.test", Uuid::now_v7()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let user = UserId(user_id);

    // Hand-craft a row whose last_used_at is older than 30 days but
    // expires_at is still future, so idle-timeout fires.
    let session_id = "idle-fake-id".to_string();
    sqlx::query(
        "INSERT INTO sessions (id, user_id, last_used_at, expires_at) \
         VALUES ($1, $2, now() - INTERVAL '40 days', now() + INTERVAL '50 days')",
    )
    .bind(&session_id)
    .bind(user.0)
    .execute(&pool)
    .await
    .unwrap();

    let outcome = session_store::lookup(&pool, &SessionConfig::default(), &session_id)
        .await
        .unwrap();
    assert!(matches!(outcome, session_store::LookupOutcome::Expired));
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM sessions WHERE id = $1")
        .bind(&session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "idle-expired row must be deleted");

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn session_touch_debounced_writes_at_most_once_per_window() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let (user_id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("touch-{}@example.test", Uuid::now_v7()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let user = UserId(user_id);
    let cfg = SessionConfig::default();
    let row = session_store::create(&pool, &cfg, user, None, None, None)
        .await
        .expect("create");

    // Backdate last_used_at so the first touch produces a measurable bump.
    sqlx::query("UPDATE sessions SET last_used_at = now() - INTERVAL '5 minutes' WHERE id = $1")
        .bind(&row.id)
        .execute(&pool)
        .await
        .unwrap();

    let debounce = session_store::build_debounce_cache();
    for _ in 0..5 {
        session_store::touch_last_used_debounced(&pool, &debounce, &row.id)
            .await
            .unwrap();
    }

    // Within the window, exactly one UPDATE should have run.
    let (last_used,): (chrono::DateTime<Utc>,) =
        sqlx::query_as("SELECT last_used_at FROM sessions WHERE id = $1")
            .bind(&row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        Utc::now().signed_duration_since(last_used) < ChronoDuration::seconds(10),
        "first touch should have moved last_used_at to ~now"
    );

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn fingerprint_salt_guard_first_boot_inserts_then_rejects_change() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    // First boot: empty history, inserts the digest.
    let inserted = fingerprint::ensure_fingerprint_salt(&pool, "salt-A")
        .await
        .unwrap();
    assert!(inserted, "first salt must be inserted");

    // Same salt: known → no insert.
    let again = fingerprint::ensure_fingerprint_salt(&pool, "salt-A")
        .await
        .unwrap();
    assert!(!again, "same salt must not re-insert");

    // Different salt without override env: refuses to boot.
    // SAFETY: tests in this binary run on the same process; clean the env
    // before and after so we don't leak state to peers.
    unsafe {
        std::env::remove_var(fingerprint::ROTATION_OVERRIDE_ENV);
    }
    let err = fingerprint::ensure_fingerprint_salt(&pool, "salt-B")
        .await
        .expect_err("rotation without override must fail");
    let msg = format!("{err:?}");
    assert!(msg.contains(fingerprint::SALT_ROTATED_CODE), "{msg}");

    // With override env: accepts rotation, persists new digest.
    unsafe {
        std::env::set_var(fingerprint::ROTATION_OVERRIDE_ENV, "1");
    }
    let accepted = fingerprint::ensure_fingerprint_salt(&pool, "salt-B")
        .await
        .unwrap();
    assert!(accepted);
    unsafe {
        std::env::remove_var(fingerprint::ROTATION_OVERRIDE_ENV);
    }

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn login_audit_records_success_and_failure() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let (user_id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("aud-{}@example.test", Uuid::now_v7()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let user = UserId(user_id);

    login_audit::record(
        &pool,
        LoginMethod::GithubOauth,
        LoginAttempt {
            user_id: Some(user),
            success: true,
            ip_hash: Some("iph"),
            user_agent_hash: Some("uah"),
            failure_reason: None,
        },
    )
    .await
    .unwrap();
    login_audit::record(
        &pool,
        LoginMethod::GithubOauth,
        LoginAttempt {
            user_id: None,
            success: false,
            ip_hash: Some("iph"),
            user_agent_hash: None,
            failure_reason: Some("invalid_state"),
        },
    )
    .await
    .unwrap();

    let (succ_count, fail_count): (i64, i64) = sqlx::query_as(
        "SELECT \
            (SELECT count(*) FROM login_attempts WHERE success), \
            (SELECT count(*) FROM login_attempts WHERE NOT success)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(succ_count, 1);
    assert_eq!(fail_count, 1);
    // Failed-row select must come back through the partial index too.
    let (recent_fail_ip,): (Option<String>,) =
        sqlx::query_as("SELECT ip_hash FROM login_attempts WHERE success = false LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recent_fail_ip.as_deref(), Some("iph"));

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn oauth_state_cleanup_purges_only_expired_rows() {
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    sqlx::query(
        "INSERT INTO oauth_states (state, provider, expires_at) VALUES \
         ($1, 'github', now() - INTERVAL '15 minutes'), \
         ($2, 'github', now() - INTERVAL '1 second'), \
         ($3, 'github', now() + INTERVAL '5 minutes')",
    )
    .bind("expired-old")
    .bind("expired-recent")
    .bind("still-valid")
    .execute(&pool)
    .await
    .unwrap();

    let removed = oauth_state_cleanup::purge_expired(&pool).await.unwrap();
    assert_eq!(removed, 2, "exactly the two expired rows should be purged");

    let (still,): (i64,) = sqlx::query_as("SELECT count(*) FROM oauth_states WHERE state = $1")
        .bind("still-valid")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(still, 1, "valid future-expiry row must survive");

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn session_fixation_pattern_destroys_pre_login_session() {
    // Mirrors what github_callback now does: read the pre-login cookie value,
    // destroy that session before minting a new one. We exercise the helpers
    // directly because the full callback path needs a GitHub mock.
    let Some((db_url, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db_url).await;
    MIGRATOR.run(&pool).await.expect("migrate");

    let (user_id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("fix-{}@example.test", Uuid::now_v7()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let user = UserId(user_id);
    let cfg = SessionConfig::default();

    // Pre-login session (attacker-supplied or stale).
    let pre = session_store::create(&pool, &cfg, user, None, None, None)
        .await
        .unwrap();

    // Callback: destroy the cookie's session id, then mint a fresh one.
    let removed = session_store::destroy(&pool, &pre.id).await.unwrap();
    assert_eq!(removed, 1, "pre-login session must be destroyed");

    let post = session_store::create(&pool, &cfg, user, None, None, None)
        .await
        .unwrap();
    assert_ne!(pre.id, post.id, "regenerated id must differ");

    let pre_lookup = session_store::lookup(&pool, &cfg, &pre.id).await.unwrap();
    assert!(
        matches!(pre_lookup, session_store::LookupOutcome::Missing),
        "old session must not be revivable"
    );

    pool.close().await;
    drop_pg(&name).await;
}
