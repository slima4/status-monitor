//! Drives the SQL month-partition maintenance for the time-retention tables.

use anyhow::Context;
use sqlx::PgPool;

use crate::error::Result;

pub const PARTITIONED_TABLES: &[&str] = &[
    "login_attempts",
    "quota_events",
    "org_audit_log",
    "credential_events",
];

/// Months provisioned ahead, so a lagging maintenance run still has a partition.
const MONTHS_AHEAD: i32 = 3;
const MONTHS_BACK: i32 = 1;

/// Create any missing month partitions for every partitioned table. Idempotent.
pub async fn ensure_partitions(pool: &PgPool) -> Result<()> {
    for table in PARTITIONED_TABLES {
        sqlx::query("SELECT ensure_month_partitions($1::regclass, $2, $3)")
            .bind(table)
            .bind(MONTHS_BACK)
            .bind(MONTHS_AHEAD)
            .execute(pool)
            .await
            .with_context(|| format!("ensure partitions for {table}"))?;
    }
    Ok(())
}

/// Drop whole partitions of `table` older than `days`. Returns how many dropped.
pub async fn drop_old_partitions(pool: &PgPool, table: &str, days: u32) -> Result<u64> {
    let (dropped,): (i32,) = sqlx::query_as(
        "SELECT drop_old_month_partitions($1::regclass, now() - ($2::bigint * INTERVAL '1 day'))",
    )
    .bind(table)
    .bind(i64::from(days))
    .fetch_one(pool)
    .await
    .with_context(|| format!("drop old partitions for {table}"))?;
    Ok(dropped as u64)
}

/// Row count in `table`'s DEFAULT partition. Non-zero means months were never
/// carved (maintenance lagged or a backdated insert) — those rows escape
/// partition pruning, so the daily tick exports this as an alertable gauge.
pub async fn default_partition_rows(pool: &PgPool, table: &str) -> Result<i64> {
    // `table` is a static entry of PARTITIONED_TABLES, never user input.
    let sql = format!("SELECT count(*) FROM {table}_default");
    let (n,): (i64,) = sqlx::query_as(&sql)
        .fetch_one(pool)
        .await
        .with_context(|| format!("count default partition rows for {table}"))?;
    Ok(n)
}
