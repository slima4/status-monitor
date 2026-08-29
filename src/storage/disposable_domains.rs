//! Persistence for the disposable-email corpus.
//!
//! The set only ever moves as a whole: the refresh job validates a candidate,
//! then replaces every row in one transaction. Nothing reads this table per
//! request — [`load_all`] runs at boot and after each refresh to fill the
//! in-memory [`crate::security::EmailPolicy`].

use std::collections::HashSet;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::Result;

/// Rows per INSERT. The corpus is ~75k domains; one statement that wide risks
/// the 65535 bind-parameter ceiling, and UNNEST keeps it to a single bind.
const CHUNK: usize = 10_000;

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub fetched_at: DateTime<Utc>,
    pub domain_count: i32,
}

pub async fn load_all(pool: &PgPool) -> Result<HashSet<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT domain FROM disposable_email_domains")
        .fetch_all(pool)
        .await
        .context("disposable_domains::load_all")?;
    Ok(rows.into_iter().map(|(d,)| d).collect())
}

pub async fn last_snapshot(pool: &PgPool) -> Result<Option<Snapshot>> {
    let row: Option<(DateTime<Utc>, i32)> =
        sqlx::query_as("SELECT fetched_at, domain_count FROM disposable_email_refresh")
            .fetch_optional(pool)
            .await
            .context("disposable_domains::last_snapshot")?;
    Ok(row.map(|(fetched_at, domain_count)| Snapshot {
        fetched_at,
        domain_count,
    }))
}

/// Swap the whole corpus and stamp the snapshot, atomically. A reader that
/// beats the commit sees the previous set in full — there is no window in
/// which the table is empty.
pub async fn replace_all(pool: &PgPool, domains: &HashSet<String>) -> Result<()> {
    let all: Vec<&str> = domains.iter().map(String::as_str).collect();
    let mut tx = pool.begin().await.context("replace_all: begin")?;
    sqlx::query("DELETE FROM disposable_email_domains")
        .execute(&mut *tx)
        .await
        .context("replace_all: clear")?;
    for chunk in all.chunks(CHUNK) {
        sqlx::query(
            "INSERT INTO disposable_email_domains (domain) \
             SELECT unnest($1::text[]) ON CONFLICT DO NOTHING",
        )
        .bind(chunk)
        .execute(&mut *tx)
        .await
        .context("replace_all: insert")?;
    }
    sqlx::query(
        "INSERT INTO disposable_email_refresh (id, fetched_at, domain_count) \
         VALUES (true, now(), $1) \
         ON CONFLICT (id) DO UPDATE SET fetched_at = now(), domain_count = EXCLUDED.domain_count",
    )
    .bind(all.len() as i32)
    .execute(&mut *tx)
    .await
    .context("replace_all: stamp")?;
    tx.commit().await.context("replace_all: commit")?;
    Ok(())
}
