//! GDPR account endpoints: data export, deletion, recovery.
//!
//! - `GET  /api/v1/me/data-export`      — everything we hold about the caller.
//! - `DELETE /api/v1/me`                — soft-delete + 30-day grace.
//! - `POST /api/v1/auth/recover-account` — undo within the grace window.
//!
//! Cross-user / cross-org redaction is deliberate here: the export is the one
//! place that legitimately reads other members' rows, so it never reuses the
//! operator-UI listing helpers (which return co-members' email). Credentials
//! are scrubbed via [`RedactedTarget`] — handing this path a decryptable
//! `Target` is a compile error, not a review miss.

use axum::Json;
use axum::extract::State;
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tower_cookies::Cookies;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::app::AppState;
use crate::auth::login_audit::{self, LoginAttempt, LoginMethod};
use crate::auth::url::token_link;
use crate::auth::{account, fingerprint, session as session_store};
use crate::domain::{UserId, strip_served_stale};
use crate::email::{EmailAddress, EmailTemplate, TransactionalEmail};
use crate::error::{AppError, Result};
use crate::storage::postgres_secrets::{RawTargetRow, RedactedTarget};
use crate::web::CurrentUser;

// ---------------------------------------------------------------------------
// Data export
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct UserDataExport {
    pub exported_at: DateTime<Utc>,
    pub user: UserExport,
    pub oauth_identities: Vec<OAuthIdentityExport>,
    pub sessions: Vec<SessionMetadata>,
    pub api_tokens: Vec<ApiTokenMetadata>,
    pub owned_orgs: Vec<OwnedOrgExport>,
    pub memberships: Vec<MembershipExport>,
    pub login_history: Vec<LoginAttemptExport>,
    pub audit_entries: Vec<AuditEntryExport>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct UserExport {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct OAuthIdentityExport {
    pub provider: String,
    pub provider_username: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_login_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct SessionMetadata {
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct ApiTokenMetadata {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OwnedOrgExport {
    pub organization: OrgExport,
    pub targets: Vec<RedactedTarget>,
    pub incidents: Vec<IncidentExport>,
    pub maintenance_windows: Vec<MaintenanceExport>,
    pub members: Vec<MemberMetadata>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct OrgExport {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct IncidentExport {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub severity: String,
    pub status_at_start: String,
    pub check_count: i32,
    pub error_sample: Option<String>,
    pub duration_secs: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct MaintenanceExport {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Co-member of an owned org. Name + role ONLY — never email. Other members'
/// addresses are third-party PII and must not leak through one user's export.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct MemberMetadata {
    pub display_name: Option<String>,
    pub role: String,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct MembershipExport {
    #[schema(value_type = String, format = "uuid")]
    pub org_id: Uuid,
    pub slug: String,
    pub role: String,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct LoginAttemptExport {
    pub method: String,
    pub success: bool,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub failure_reason: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AuditEntryExport {
    #[schema(value_type = String, format = "uuid")]
    pub org_id: Uuid,
    pub action: String,
    #[schema(value_type = Object)]
    pub metadata: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}

#[utoipa::path(
    get,
    path = "/api/v1/me/data-export",
    tag = "account",
    summary = "Export all personal data associated with the calling user",
    description = "Returns a JSON document containing all data linked to the \
                   authenticated user: account info, OAuth identities, session \
                   and API-token metadata (never raw values), owned orgs with \
                   their targets (credentials redacted), incidents and \
                   maintenance, memberships, login history and audit entries. \
                   Excludes data from orgs where the user is a member but not \
                   owner.",
    responses(
        (status = 200, body = UserDataExport),
        (status = 401, body = ApiError),
    ),
)]
pub async fn data_export(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Response> {
    let pool = state.require_db()?;
    let export = build_export(pool, user_id).await?;
    let filename = format!(
        "status-monitor-export-{}-{}.json",
        user_id.0,
        Utc::now().format("%Y-%m-%d")
    );
    let body = serde_json::to_vec_pretty(&export)
        .map_err(|e| AppError::Other(anyhow::anyhow!("data-export serialize: {e}")))?;
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response())
}

/// Build the export. Export-specific queries deliberately do NOT inherit the
/// universal `deleted_at IS NULL` filter — within the purge grace a
/// soft-deleted org is still personal data the controller holds, so it is
/// included and stamped with its `deleted_at`.
async fn build_export(pool: &sqlx::PgPool, user_id: UserId) -> Result<UserDataExport> {
    let user: UserExport = sqlx::query_as(
        "SELECT id, email::text AS email, display_name, created_at, updated_at, deleted_at \
         FROM users WHERE id = $1",
    )
    .bind(user_id.0)
    .fetch_optional(pool)
    .await
    .map_err(db_err("user"))?
    .ok_or(AppError::Unauthorized)?;

    let oauth_identities: Vec<OAuthIdentityExport> = sqlx::query_as(
        "SELECT provider, provider_username, created_at, last_login_at \
         FROM oauth_identities WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("oauth_identities"))?;

    let sessions: Vec<SessionMetadata> = sqlx::query_as(
        "SELECT created_at, last_used_at, expires_at, ip_hash, user_agent_hash \
         FROM sessions WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("sessions"))?;

    let api_tokens: Vec<ApiTokenMetadata> = sqlx::query_as(
        "SELECT id, name, created_at, last_used_at, expires_at \
         FROM api_tokens WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("api_tokens"))?;

    // Orgs the caller owns — including soft-deleted-but-not-purged ones.
    let owned: Vec<OrgExport> = sqlx::query_as(
        "SELECT o.id, o.slug::text AS slug, o.name, o.created_at, o.updated_at, o.deleted_at \
         FROM organizations o \
         JOIN memberships m ON m.org_id = o.id \
         WHERE m.user_id = $1 AND m.role = 'owner' \
         ORDER BY o.created_at",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("owned_orgs"))?;

    let mut owned_orgs = Vec::with_capacity(owned.len());
    for org in owned {
        owned_orgs.push(build_owned_org(pool, org).await?);
    }

    // Memberships where the user is NOT an owner (owner orgs are exported in
    // full above). org_id + slug + role only.
    let memberships: Vec<MembershipExport> = sqlx::query_as(
        "SELECT o.id AS org_id, o.slug::text AS slug, m.role \
         FROM memberships m \
         JOIN organizations o ON o.id = m.org_id \
         WHERE m.user_id = $1 AND m.role <> 'owner' \
         ORDER BY o.created_at",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("memberships"))?;

    let login_history: Vec<LoginAttemptExport> = sqlx::query_as(
        "SELECT method, success, ip_hash, user_agent_hash, failure_reason, occurred_at \
         FROM login_attempts \
         WHERE user_id = $1 AND occurred_at > now() - INTERVAL '90 days' \
         ORDER BY occurred_at DESC",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("login_history"))?;

    let audit_entries: Vec<AuditEntryExport> = sqlx::query_as(
        "SELECT org_id, action, metadata, occurred_at \
         FROM org_audit_log WHERE actor_id = $1 ORDER BY occurred_at DESC",
    )
    .bind(user_id.0)
    .fetch_all(pool)
    .await
    .map_err(db_err("audit_entries"))?;

    Ok(UserDataExport {
        exported_at: Utc::now(),
        user,
        oauth_identities,
        sessions,
        api_tokens,
        owned_orgs,
        memberships,
        login_history,
        audit_entries,
    })
}

async fn build_owned_org(pool: &sqlx::PgPool, org: OrgExport) -> Result<OwnedOrgExport> {
    // Raw target columns — `check_spec` is whatever sits at rest (encrypted
    // envelope or no-KEK plaintext) and is NEVER decrypted; `from_row`
    // redacts it. The org's `deleted_at` stamps every target, since targets
    // carry no soft-delete column of their own.
    let target_rows: Vec<RawTargetRow> = sqlx::query_as(
        "SELECT id, name, check_spec, interval_secs, enabled, tags, \
                group_name, owner_user_id, \
                public_status, public_name, public_description, public_group, \
                public_sort_order, created_at, updated_at \
         FROM targets WHERE org_id = $1 ORDER BY created_at",
    )
    .bind(org.id)
    .fetch_all(pool)
    .await
    .map_err(db_err("targets"))?;
    let targets = target_rows
        .into_iter()
        .map(|row| RedactedTarget::from_row(row, org.deleted_at))
        .collect();

    let mut incidents: Vec<IncidentExport> = sqlx::query_as(
        "SELECT id, target_id, started_at, ended_at, severity, status_at_start, \
                check_count, error_sample, duration_secs, created_at, updated_at \
         FROM incidents WHERE org_id = $1 ORDER BY started_at DESC",
    )
    .bind(org.id)
    .fetch_all(pool)
    .await
    .map_err(db_err("incidents"))?;
    for inc in &mut incidents {
        if let Some(e) = inc.error_sample.take() {
            inc.error_sample = strip_served_stale(&e).map(str::to_owned);
        }
    }

    let maintenance_windows: Vec<MaintenanceExport> = sqlx::query_as(
        "SELECT id, title, description, starts_at, ends_at, created_at, updated_at \
         FROM maintenance_windows WHERE org_id = $1 ORDER BY starts_at DESC",
    )
    .bind(org.id)
    .fetch_all(pool)
    .await
    .map_err(db_err("maintenance_windows"))?;

    // Co-members: display_name + role ONLY. A dedicated query, never the
    // operator-UI `list_members` helper (that returns email — third-party
    // PII the export must not leak).
    let members: Vec<MemberMetadata> = sqlx::query_as(
        "SELECT u.display_name, m.role \
         FROM memberships m JOIN users u ON u.id = m.user_id \
         WHERE m.org_id = $1 ORDER BY m.created_at",
    )
    .bind(org.id)
    .fetch_all(pool)
    .await
    .map_err(db_err("members"))?;

    Ok(OwnedOrgExport {
        organization: org,
        targets,
        incidents,
        maintenance_windows,
        members,
    })
}

fn db_err(what: &'static str) -> impl Fn(sqlx::Error) -> AppError {
    move |e| AppError::Other(anyhow::anyhow!("data-export {what}: {e}"))
}

// ---------------------------------------------------------------------------
// Account deletion
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct DeletionConfirmation {
    pub scheduled_purge_at: DateTime<Utc>,
    /// Masked, e.g. `j*****@gmail.com`.
    pub recovery_token_sent_to_email: String,
    pub can_recover_until: DateTime<Utc>,
}

#[utoipa::path(
    delete,
    path = "/api/v1/me",
    tag = "account",
    summary = "Delete the calling user's account (soft delete + 30-day grace)",
    description = "Immediately deactivates the account and schedules a \
                   permanent purge after the grace period. Cancels sessions, \
                   deletes API tokens, declines pending invitations, and \
                   tombstones organisations the user solely owns. Rejects with \
                   422 OWNS_SHARED_ORGS if the user solely owns organisations \
                   that still have other members. A recovery link is emailed \
                   at deletion time.",
    responses(
        (status = 200, body = DeletionConfirmation),
        (status = 409, body = ApiError, description = "Account already scheduled for deletion"),
        (status = 422, body = ApiError, description = "User solely owns orgs with other members"),
    ),
)]
pub async fn delete_account(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<DeletionConfirmation>> {
    let pool = state.require_db()?;
    let grace_days = state.cfg.tenancy.deletion_grace_period_days;

    let outcome = account::request_deletion(pool, user_id, grace_days).await?;

    // Email post-commit: a mail failure must not roll back the deletion the
    // user asked for. The recovery row is already persisted (hashed); the raw
    // token lives only in this stack frame to build the link.
    let recovery_url = token_link(
        &state.cfg.auth.public_base_url,
        "/recover-account",
        &outcome.recovery_token,
    );
    let outgoing = TransactionalEmail {
        from: EmailAddress::new(
            state.cfg.email.from_address.clone(),
            state.cfg.email.from_name.clone(),
        ),
        to: EmailAddress::new(outcome.email.clone(), outcome.email.clone()),
        template: EmailTemplate::AccountDeletion {
            recovery_url,
            scheduled_purge_at: outcome.grace_deadline,
        },
    };
    if let Err(err) = state.email_sender.send(outgoing).await {
        tracing::warn!(error = %err, "account-deletion confirmation email send failed");
    }

    Ok(Json(DeletionConfirmation {
        scheduled_purge_at: outcome.grace_deadline,
        recovery_token_sent_to_email: mask_email(&outcome.email),
        can_recover_until: outcome.grace_deadline,
    }))
}

// ---------------------------------------------------------------------------
// Account recovery
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct RecoverRequest {
    pub token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecoveredAccount {
    #[schema(value_type = String, format = "uuid")]
    pub user_id: Uuid,
    pub email: String,
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/recover-account",
    tag = "account",
    summary = "Undo an account deletion within the grace window",
    request_body = RecoverRequest,
    responses(
        (status = 200, body = RecoveredAccount, description = "Account restored; fresh session issued"),
        (status = 404, body = ApiError, description = "Recovery link invalid or expired"),
        (status = 410, body = ApiError, description = "Account already permanently purged"),
    ),
)]
pub async fn recover_account(
    State(state): State<AppState>,
    crate::web::client_ip::ClientIp(client_ip): crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
    Json(body): Json<RecoverRequest>,
) -> Result<Response> {
    let pool = state.require_db()?;
    let outcome = account::recover(pool, body.token.trim()).await?;

    // Account is un-deleted and committed; `load_user` will now resolve it,
    // so a session minted here is valid (the load-bearing un-delete-before-
    // session order).
    let salt = state.cfg.auth.fingerprint_salt.as_str();
    let ip_hash = fingerprint::hash_fingerprint(salt, &client_ip.to_string());
    let ua_hash = fingerprint::hash_fingerprint(
        salt,
        headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
    );

    // Session fixation: drop any pre-existing cookie-bound session first.
    let cookie_name = state.cfg.auth.session.cookie_name.as_str();
    if let Some(prev) = cookies.get(cookie_name).map(|c| c.value().to_string())
        && !prev.is_empty()
        && let Err(err) = session_store::destroy(pool, &prev).await
    {
        tracing::warn!(error = %err, "recover: pre-existing session destroy failed");
    }

    let created = session_store::create(
        pool,
        &state.cfg.auth.session,
        outcome.user_id,
        None,
        ip_hash.as_deref(),
        ua_hash.as_deref(),
    )
    .await?;

    if let Err(err) = login_audit::record(
        pool,
        LoginMethod::MagicLink,
        LoginAttempt {
            user_id: Some(outcome.user_id),
            success: true,
            ip_hash: ip_hash.as_deref(),
            user_agent_hash: ua_hash.as_deref(),
            failure_reason: None,
        },
    )
    .await
    {
        tracing::warn!(error = %err, "recover: login_audit write failed (non-fatal)");
    }

    cookies.add(session_store::build_cookie(
        &state.cfg.auth.session,
        created.cookie_token,
    ));
    if let Err(err) = crate::web::theme::issue_for(&state, &cookies, outcome.user_id).await {
        tracing::warn!(error = %err, "theme cookie issue failed (non-fatal)");
    }

    Ok(Json(RecoveredAccount {
        user_id: outcome.user_id.0,
        email: outcome.email,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `jane@gmail.com` → `j*****@gmail.com`. Keeps the first local-part char and
/// the full domain so the user can recognise which inbox to check without the
/// response disclosing the whole address.
fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let first = local.chars().next().unwrap_or('*');
            format!("{first}*****@{domain}")
        }
        None => "*****".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::mask_email;

    #[test]
    fn masks_local_part_keeps_domain() {
        assert_eq!(mask_email("jane@gmail.com"), "j*****@gmail.com");
        assert_eq!(mask_email("a@x.io"), "a*****@x.io");
    }

    #[test]
    fn malformed_email_fully_masked() {
        assert_eq!(mask_email("not-an-email"), "*****");
    }
}
