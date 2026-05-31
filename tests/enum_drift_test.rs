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

use uptimepage::auth::OauthProvider;
use uptimepage::domain::{
    AppTheme, ChannelKind, IncidentSeverity, IncidentStatusPhase, PublicStyle,
};

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

#[tokio::test]
#[ignore]
async fn oauth_identities_provider_check_matches_rust_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "oauth_identities_provider_check")
        .await
        .expect("oauth_identities_provider_check missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(
        OauthProvider::ALL
            .iter()
            .map(|p| p.as_db_str().to_string())
            .collect(),
    );
    assert_eq!(
        db, rust,
        "oauth_identities.provider CHECK list ({db:?}) drifted from OauthProvider ({rust:?})"
    );
}

/// `oauth_states.provider` must accept the same provider set as
/// `oauth_identities.provider` — the OAuth dance writes a state row keyed
/// on a provider before the callback ever inserts an identity row, so a
/// CHECK on one but not the other would let a new provider's first request
/// 500 at callback time instead of being rejected up-front by the schema.
#[tokio::test]
#[ignore]
async fn oauth_states_provider_check_matches_rust_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "oauth_states_provider_check")
        .await
        .expect("oauth_states_provider_check missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(
        OauthProvider::ALL
            .iter()
            .map(|p| p.as_db_str().to_string())
            .collect(),
    );
    assert_eq!(
        db, rust,
        "oauth_states.provider CHECK list ({db:?}) drifted from OauthProvider ({rust:?})"
    );
}

#[tokio::test]
#[ignore]
async fn notification_channels_kind_check_matches_rust_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "notification_channels_kind_check")
        .await
        .expect("notification_channels_kind_check missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(
        ChannelKind::ALL
            .iter()
            .map(|k| k.as_db_str().to_string())
            .collect(),
    );
    assert_eq!(
        db, rust,
        "notification_channels.kind CHECK list ({db:?}) drifted from ChannelKind ({rust:?})"
    );
}

#[tokio::test]
#[ignore]
async fn users_theme_check_matches_app_theme_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "users_theme_check")
        .await
        .expect("users_theme_check missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(AppTheme::ALL.iter().map(|s| (*s).to_string()).collect());
    assert_eq!(
        db, rust,
        "users.theme CHECK list ({db:?}) drifted from AppTheme ({rust:?})"
    );
}

#[tokio::test]
#[ignore]
async fn status_pages_public_style_check_matches_public_style_enum() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let def = constraint_def(&pool, "status_page_style_known")
        .await
        .expect("status_page_style_known missing");
    let db = sorted(quoted_tokens(&def));
    let rust = sorted(PublicStyle::ALL.iter().map(|s| (*s).to_string()).collect());
    assert_eq!(
        db, rust,
        "status_pages.public_style CHECK list ({db:?}) drifted from PublicStyle ({rust:?})"
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
