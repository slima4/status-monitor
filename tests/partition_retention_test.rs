//! Live-PG tests for month-partitioned audit tables: inserts route to a
//! concrete partition (not the DEFAULT backstop), ids are time-ordered v7, and
//! aged-out partitions are dropped while the current month survives.
//! Skipped by default; run under `--run-ignored` with `DATABASE_URL` set.

mod common;

use uptimepage::storage::partitions::{self, PARTITIONED_TABLES};

async fn scalar<T>(pool: &sqlx::PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    let (v,): (T,) = sqlx::query_as(sql).fetch_one(pool).await.expect("scalar");
    v
}

#[tokio::test]
#[ignore]
async fn provisions_partitions_routes_inserts_and_drops_aged() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };

    partitions::ensure_partitions(&pool).await.expect("ensure");

    for table in PARTITIONED_TABLES {
        // Parent + DEFAULT + provisioned months → several concrete children.
        let children: i64 = scalar(
            &pool,
            &format!(
                "SELECT count(*) FROM pg_inherits WHERE inhparent = '{table}'::regclass"
            ),
        )
        .await;
        assert!(children >= 2, "{table} should have month partitions, got {children}");
    }

    // A fresh login_attempt lands in the current-month partition, never DEFAULT.
    let part: String = scalar(
        &pool,
        "WITH ins AS (
             INSERT INTO login_attempts (method, success) VALUES ('parttest', true)
             RETURNING tableoid, id
         )
         SELECT ins.tableoid::regclass::text || '|' || ins.id::text FROM ins",
    )
    .await;
    let (partition, id) = part.split_once('|').unwrap();
    assert!(
        partition.starts_with("login_attempts_p"),
        "insert routed to {partition}, expected a concrete month partition"
    );
    // RFC 9562 v7: the 13th hex digit (version nibble) is 7.
    assert_eq!(
        &id.chars().nth(14).unwrap().to_string(),
        "7",
        "id {id} is not a v7 uuid"
    );

    // Drop everything strictly before the current month: past partitions go,
    // the current month (and its just-inserted row) stays.
    let before: i64 =
        scalar(&pool, "SELECT count(*) FROM login_attempts WHERE method = 'parttest'").await;
    assert_eq!(before, 1);
    partitions::drop_old_partitions(&pool, "login_attempts", 0)
        .await
        .expect("drop");
    let after: i64 =
        scalar(&pool, "SELECT count(*) FROM login_attempts WHERE method = 'parttest'").await;
    assert_eq!(after, 1, "current-month row must survive a drop of aged partitions");

    let _ = sqlx::query("DELETE FROM login_attempts WHERE method = 'parttest'")
        .execute(&pool)
        .await;
}
