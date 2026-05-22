//! Live-PG assertion that every Rust enum whose values are written into a
//! Postgres column with a closed `CHECK (col IN (…))` constraint stays in
//! lockstep with that constraint's value list. Adding a new variant on the
//! Rust side without the paired migration would otherwise compile, pass the
//! in-memory test stores, and 500 the first INSERT that exercises the new
//! variant in production.
//!
//! Skipped by default; runs under `--include-ignored` once `DATABASE_URL`
//! is set. The test harness auto-applies all Postgres migrations on first
//! connect, so the introspection sees the same constraint defs prod runs.

mod common;

use status_monitor::domain::{IncidentSeverity, IncidentStatusPhase};

/// Pull the parenthesised list from a constraint def like
/// `CHECK ((severity = ANY (ARRAY['minor'::text, 'major'::text, 'critical'::text])))`
/// or `CHECK ((severity IN ('minor', 'major', 'critical')))`. Postgres
/// normalises the printed form across versions, so we walk the string for
/// single-quoted tokens rather than matching the surrounding shape.
fn quoted_tokens(def: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = def.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            continue;
        }
        let mut s = String::new();
        loop {
            match chars.next() {
                Some('\'') => {
                    if chars.peek() == Some(&'\'') {
                        s.push('\'');
                        chars.next();
                    } else {
                        break;
                    }
                }
                Some(c) => s.push(c),
                None => break,
            }
        }
        out.push(s);
    }
    out
}

async fn constraint_def(pool: &sqlx::PgPool, conname: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = $1",
    )
    .bind(conname)
    .fetch_optional(pool)
    .await
    .expect("query pg_constraint")
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
#[ignore]
async fn incidents_severity_check_matches_rust_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "incidents_severity_check")
        .await
        .expect("incidents_severity_check missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(
        IncidentSeverity::ALL
            .iter()
            .map(|s| s.as_db_str().to_string())
            .collect(),
    );
    assert_eq!(
        db, rust,
        "incidents.severity CHECK list ({db:?}) drifted from IncidentSeverity ({rust:?})"
    );
}

#[tokio::test]
#[ignore]
async fn incident_updates_phase_check_matches_rust_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "incident_updates_phase_check")
        .await
        .expect("incident_updates_phase_check missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(
        IncidentStatusPhase::ALL
            .iter()
            .map(|p| p.as_db_str().to_string())
            .collect(),
    );
    assert_eq!(
        db, rust,
        "incident_updates.phase CHECK list ({db:?}) drifted from IncidentStatusPhase ({rust:?})"
    );
}

/// `incidents.status_at_start` is a *subset* of `CheckStatus` (the values
/// that can open an incident — never 'up'), so it can't reuse the enum
/// directly. Pin the literal set so a future migration that adds, say,
/// 'maintenance' must update this test in the same diff.
#[tokio::test]
#[ignore]
async fn incidents_status_at_start_check_is_pinned() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "incidents_status_at_start_check")
        .await
        .expect("incidents_status_at_start_check missing");
    let db = sorted(quoted_tokens(&def));
    let expected = sorted(vec!["down".into(), "degraded".into(), "error".into()]);
    assert_eq!(
        db, expected,
        "incidents.status_at_start CHECK list ({db:?}) drifted from the pinned set ({expected:?})"
    );
}
