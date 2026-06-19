//! Named, process-generated secrets persisted across restarts.

use anyhow::Context;
use sqlx::PgPool;

use crate::auth::token_hash::generate_raw_token;
use crate::error::Result;

/// Return the secret named `name`, generating and persisting a fresh one on
/// first call. The generate races harmlessly: the unique `name` plus
/// `ON CONFLICT DO NOTHING` means only the first writer's value lands, and the
/// follow-up SELECT always returns the winner.
pub async fn ensure_secret(pool: &PgPool, name: &str) -> Result<String> {
    sqlx::query("INSERT INTO app_secrets (name, secret) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(name)
        .bind(generate_raw_token())
        .execute(pool)
        .await
        .context("app_secrets::ensure_secret insert")?;
    let secret: String = sqlx::query_scalar("SELECT secret FROM app_secrets WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .context("app_secrets::ensure_secret select")?;
    Ok(secret)
}
