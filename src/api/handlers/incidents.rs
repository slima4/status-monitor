//! Operator endpoints for incident narration and timeline updates.
//!
//! Both routes read/write the materialised `incidents` + `incident_updates`
//! tables via `IncidentNarrationStore` — disjoint from the background writer
//! (`public_status::incident_writer`) which only opens/closes incidents based
//! on check results.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::AppendHeaders;
use uuid::Uuid;

use crate::api::ApiError;
use crate::api::error::codes;
use crate::api::handlers::validation::{self, validate_message};
use crate::app::AppState;
use crate::domain::{Incident, IncidentNarrationUpdate, NewIncidentUpdate, PublicIncidentUpdate};
use crate::error::{AppError, Result};

#[utoipa::path(
    patch,
    path = "/api/v1/incidents/{id}",
    tag = "incidents",
    summary = "Update incident narration (public title, description, severity)",
    description = "Sending JSON `null` for `public_title` or `public_description` clears the \
                   stored value; omitting the field leaves it unchanged. The public page falls \
                   back to auto-generated content when the title is null.",
    params(("id" = Uuid, Path)),
    request_body(content = IncidentNarrationUpdate, example = json!({
        "public_title": "Latency spike on EU API",
        "public_description": "Investigation in progress.",
        "severity": "major"
    })),
    responses(
        (status = 200, body = Incident),
        (status = 400, body = ApiError),
        (status = 404, body = ApiError),
    ),
)]
pub async fn update_incident_narration(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(update): Json<IncidentNarrationUpdate>,
) -> Result<Json<Incident>> {
    validate_optional_title(update.public_title.as_ref())?;
    validate_optional_description(update.public_description.as_ref())?;
    match state
        .incident_narration_store
        .patch_narration(id, update)
        .await?
    {
        Some(inc) => Ok(Json(inc)),
        None => Err(AppError::not_found(
            codes::INCIDENT_NOT_FOUND,
            "incident not found",
        )),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/incidents/{id}/updates",
    tag = "incidents",
    summary = "Post a status update to an incident timeline",
    description = "Appends an operator-authored entry to the incident's public update timeline. \
                   Setting `phase=resolved` does NOT end the incident automatically — that's \
                   driven by check-result transitions. Posting an update on an already-ended \
                   incident is allowed (useful for postmortems).",
    params(("id" = Uuid, Path)),
    request_body(content = NewIncidentUpdate, example = json!({
        "phase": "identified",
        "message": "Rolled back the offending deploy. Verifying recovery."
    })),
    responses(
        (status = 201, body = PublicIncidentUpdate,
            headers(("Location" = String))),
        (status = 400, body = ApiError),
        (status = 404, body = ApiError),
    ),
)]
pub async fn post_incident_update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(new): Json<NewIncidentUpdate>,
) -> Result<(
    StatusCode,
    AppendHeaders<[(axum::http::HeaderName, HeaderValue); 1]>,
    Json<PublicIncidentUpdate>,
)> {
    validate_message(&new.message, "message")?;
    let entry = state
        .incident_narration_store
        .append_update(id, new, None)
        .await?
        .ok_or_else(|| AppError::not_found(codes::INCIDENT_NOT_FOUND, "incident not found"))?;
    let location =
        HeaderValue::from_str(&format!("/api/v1/incidents/{id}")).expect("uuid ascii");
    Ok((
        StatusCode::CREATED,
        AppendHeaders([(header::LOCATION, location)]),
        Json(entry),
    ))
}

// ── Validation ──────────────────────────────────────────────────────────

/// Double-Option-aware title validator: leaves missing and null fields alone,
/// rejects whitespace as `EMPTY_TITLE`, length-checks present strings.
/// Whitespace is *not* a clear request — callers should send JSON `null`.
fn validate_optional_title(title: Option<&Option<String>>) -> Result<()> {
    let Some(Some(t)) = title else { return Ok(()) };
    validation::validate_title(t, "public_title")
}

fn validate_optional_description(desc: Option<&Option<String>>) -> Result<()> {
    let Some(Some(d)) = desc else { return Ok(()) };
    validation::validate_description(Some(d), "public_description")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_title_rejected_only_when_explicit_value() {
        // null clears — allowed
        assert!(validate_optional_title(Some(&None)).is_ok());
        // missing — allowed
        assert!(validate_optional_title(None).is_ok());
        // whitespace — rejected
        let bad = Some("   ".to_string());
        assert!(matches!(
            validate_optional_title(Some(&bad)),
            Err(AppError::BadRequest { code, .. }) if code == codes::EMPTY_TITLE
        ));
    }

    #[test]
    fn title_length_capped() {
        let bad = Some("x".repeat(validation::MAX_TITLE + 1));
        assert!(matches!(
            validate_optional_title(Some(&bad)),
            Err(AppError::BadRequest { code, .. }) if code == codes::TITLE_TOO_LONG
        ));
    }

    #[test]
    fn empty_message_rejected() {
        assert!(matches!(
            validate_message("", "message"),
            Err(AppError::BadRequest { code, .. }) if code == codes::EMPTY_MESSAGE
        ));
        assert!(matches!(
            validate_message("   \n\t", "message"),
            Err(AppError::BadRequest { code, .. }) if code == codes::EMPTY_MESSAGE
        ));
    }

    #[test]
    fn long_message_rejected() {
        let m = "x".repeat(validation::MAX_MESSAGE + 1);
        assert!(matches!(
            validate_message(&m, "message"),
            Err(AppError::BadRequest { code, .. }) if code == codes::MESSAGE_TOO_LONG
        ));
    }
}
