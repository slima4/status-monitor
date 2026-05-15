//! Organization invitations — owner-scoped CRUD + redeem.
//!
//! Five routes:
//! - `POST   /api/v1/orgs/{id}/invitations`     create
//! - `GET    /api/v1/orgs/{id}/invitations`     list pending
//! - `DELETE /api/v1/orgs/{id}/invitations/{i}` revoke
//! - `POST   /api/v1/invitations/accept`        accept (token in body)
//! - `POST   /api/v1/invitations/decline`       decline (token in body)
//!
//! The unauthenticated landing page `GET /invitations/accept?token=...` is a
//! separate (HTML) flow added by the onboarding UI phase; for v1 we expose only
//! the JSON endpoints. Email-sending uses [`AppState::email_sender`] so the
//! provider stays config-driven.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::email_norm;
use crate::auth::invitations as inv;
use crate::auth::url::url_encode;
use crate::domain::{OrgId, Role};
use crate::email::{EmailAddress, EmailTemplate, TransactionalEmail};
use crate::error::{AppError, Result};
use crate::storage::orgs as orgs_store;
use crate::web::CurrentUser;
use crate::web::auth::api_token::VerifiedCurrentUser;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInvitationRequest {
    pub email: String,
    pub role: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InvitationView {
    pub id: Uuid,
    pub email: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub async fn create(
    State(state): State<AppState>,
    VerifiedCurrentUser(CurrentUser(user_id)): VerifiedCurrentUser,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<InvitationView>)> {
    let pool = state.require_db()?;
    let org = OrgId(org_id);
    let email = validate_email(&req.email)?;
    let role = parse_role(&req.role)?;

    // Owner-only.
    if !orgs_store::is_owner(pool, user_id, org).await? {
        return Err(AppError::Forbidden);
    }

    // Org must exist and be active. is_owner returns false for soft-deleted
    // orgs too, but bail explicitly so the error is intelligible.
    let Some(org_row) = orgs_store::get_org(pool, org).await? else {
        return Err(AppError::not_found(codes::ORG_NOT_FOUND, "org not found"));
    };
    if org_row.deleted_at.is_some() {
        return Err(AppError::not_found(codes::ORG_DELETED, "org is deleted"));
    }

    // Already a member?
    if let Some(uid) = orgs_store::find_user_by_email(pool, email).await?
        && orgs_store::is_active_member(pool, uid, org).await?
    {
        return Err(AppError::conflict(
            codes::ALREADY_MEMBER,
            "user is already a member of this org",
        ));
    }
    // Dedupe + pending-cap are enforced atomically inside `inv::create`
    // (one transaction, per-org advisory lock) — a pre-check here would
    // just be a racy duplicate of the real gate.
    // Cap from the plan (single source of truth). `inv::create` enforces it
    // atomically under the per-org advisory lock — same number, one gate.
    let max = u32::try_from(
        state
            .quotas
            .limit_for_org(org)
            .await?
            .max_pending_invitations,
    )
    .unwrap_or(u32::MAX);
    let expiry_hours = state.cfg.auth.invitations.expiry_hours;
    let created = inv::create(pool, org, user_id, email, role, expiry_hours, max).await?;

    // Resolve inviter display + email for the outgoing message.
    let inviter = inviter_display(pool, user_id).await?;
    let accept_url = action_url(&state, "accept", &created.token);
    let decline_url = action_url(&state, "decline", &created.token);
    let from = EmailAddress::new(
        state.cfg.email.from_address.clone(),
        state.cfg.email.from_name.clone(),
    );
    let to = EmailAddress::new(email.to_string(), email.to_string());
    let outgoing = TransactionalEmail {
        from,
        to,
        template: EmailTemplate::Invitation {
            org_name: org_row.name,
            inviter_display: inviter,
            accept_url,
            decline_url,
            expires_at: created.row.expires_at,
        },
    };
    if let Err(err) = state.email_sender.send(outgoing).await {
        // Roll back so the recipient isn't left with a row they can never
        // redeem (no email = no token in their inbox). The DB row is the
        // only place the token-hash lives, so deleting it removes the only
        // path to acceptance.
        if let Err(rev_err) = inv::revoke(pool, org, created.row.id).await {
            tracing::warn!(error = %rev_err, "invitation rollback failed after send error");
        }
        tracing::warn!(error = %err, org = %org.0, "invitation send failed");
        return Err(AppError::Other(anyhow::anyhow!(
            "invitation send failed: {err}"
        )));
    }

    Ok((
        StatusCode::CREATED,
        Json(InvitationView {
            id: created.row.id,
            email: created.row.email,
            role: created.row.role.as_db_str().to_string(),
            created_at: created.row.created_at,
            expires_at: created.row.expires_at,
        }),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Vec<InvitationView>>> {
    let pool = state.require_db()?;
    let org = OrgId(org_id);
    if !orgs_store::is_owner(pool, user_id, org).await? {
        return Err(AppError::Forbidden);
    }
    let rows = inv::list_pending_for_org(pool, org).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| InvitationView {
                id: r.id,
                email: r.email,
                role: r.role,
                created_at: r.created_at,
                expires_at: r.expires_at,
            })
            .collect(),
    ))
}

pub async fn revoke(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Path((org_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let pool = state.require_db()?;
    let org = OrgId(org_id);
    if !orgs_store::is_owner(pool, user_id, org).await? {
        return Err(AppError::Forbidden);
    }
    let removed = inv::revoke(pool, org, invitation_id).await?;
    if !removed {
        return Err(AppError::not_found(
            codes::INVITATION_INVALID,
            "invitation not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TokenBody {
    pub token: String,
}

pub async fn accept(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Json(body): Json<TokenBody>,
) -> Result<StatusCode> {
    let pool = state.require_db()?;
    let Some(row) = inv::find_pending_by_token(pool, body.token.trim()).await? else {
        return Err(AppError::not_found(
            codes::INVITATION_INVALID,
            "invitation is invalid or has expired",
        ));
    };
    // Refuse on soft-deleted org. The owner could have soft-deleted the org
    // after the invite was sent; silently adding a membership to a tombstoned
    // org would mask itself in `list_orgs_for_user`.
    let Some(org_row) = orgs_store::get_org(pool, row.org_id).await? else {
        return Err(AppError::not_found(codes::ORG_NOT_FOUND, "org not found"));
    };
    if org_row.deleted_at.is_some() {
        return Err(AppError::not_found(codes::ORG_DELETED, "org is deleted"));
    }

    // Caller's email must match the invitation. CITEXT compared in SQL.
    let caller_email: Option<(String,)> =
        sqlx::query_as("SELECT email::text FROM users WHERE id = $1 AND deleted_at IS NULL")
            .bind(user_id.0)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("accept lookup user: {e}")))?;
    let Some((caller_email,)) = caller_email else {
        return Err(AppError::Unauthorized);
    };
    if !caller_email.eq_ignore_ascii_case(&row.email) {
        return Err(AppError::forbidden_code(
            codes::INVITATION_EMAIL_MISMATCH,
            "this invitation is for a different email address",
        ));
    }

    // State transition first so the race-loser sees `INVITATION_INVALID`
    // BEFORE a membership row is inserted. Otherwise two parallel accepts
    // both pass find_pending_by_token, both call add_member (idempotent
    // ON CONFLICT), the loser sees INVITATION_INVALID, and an orphan
    // membership remains visible to the same-user race.
    if !inv::mark_accepted(pool, row.id).await? {
        return Err(AppError::not_found(
            codes::INVITATION_INVALID,
            "invitation is invalid or has expired",
        ));
    }
    // actor = the redeeming user (self-onboard via invitation token).
    orgs_store::add_member(pool, row.org_id, user_id, user_id, row.role).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Decline doesn't require auth — anyone holding the token (the recipient
/// from the email) can decline. We never reveal whether the token matches.
pub async fn decline(
    State(state): State<AppState>,
    Json(body): Json<TokenBody>,
) -> Result<StatusCode> {
    let pool = state.require_db()?;
    let Some(row) = inv::find_pending_by_token(pool, body.token.trim()).await? else {
        return Err(AppError::not_found(
            codes::INVITATION_INVALID,
            "invitation is invalid or has expired",
        ));
    };
    if !inv::mark_declined(pool, row.id).await? {
        return Err(AppError::not_found(
            codes::INVITATION_INVALID,
            "invitation is invalid or has expired",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn parse_role(s: &str) -> Result<Role> {
    Role::from_db_str(s.trim()).ok_or_else(|| {
        AppError::bad_request_field(
            codes::INVALID_ROLE,
            "role must be 'owner' or 'member'",
            "role",
        )
    })
}

fn validate_email(raw: &str) -> Result<&str> {
    email_norm::normalize(raw).ok_or_else(|| {
        AppError::bad_request_field(
            codes::INVALID_EMAIL,
            "email must contain '@' and be 1-254 chars",
            "email",
        )
    })
}

async fn inviter_display(pool: &sqlx::PgPool, user: crate::domain::UserId) -> Result<String> {
    let row: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT display_name, email::text FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user.0)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("inviter lookup: {e}")))?;
    Ok(match row {
        Some((Some(name), email)) => format!("{name} <{email}>"),
        Some((None, email)) => email,
        None => "Someone".to_string(),
    })
}

fn action_url(state: &AppState, kind: &str, token: &str) -> String {
    let base = state.cfg.auth.public_base_url.trim_end_matches('/');
    // Path mirrors AUTH §5.6: GET /invitations/accept?token=... for the
    // landing page; the JSON endpoints accept the token in the body.
    format!("{base}/invitations/{kind}?token={}", url_encode(token))
}
