//! Operator-side public status-page settings (`/api/v1/orgs/{id}/status-page`
//! and its `/logo` sub-resource).
//!
//! Every route is owner-only and keyed by an explicit `:id` path param, in the
//! same spirit as [`super::orgs`] — a multi-org user manages each org's page
//! independently of their active org. Branding is validated through the
//! domain [`PublicOrgBranding::validate`] (the column CHECK constraints'
//! mirror) before it touches the database; the logo path is server-derived
//! from a content hash and never client-chosen.

use axum::Json;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::ApiError;
use crate::api::error::codes;
use crate::api::routes::{path_based_public_routes_enabled, subdomain_public_routes_enabled};
use crate::app::AppState;
use crate::domain::{OrgId, PublicOrgBranding};
use crate::error::{AppError, Result};
use crate::public_status::{LocalDiskLogoStorage, LogoMime, LogoStorage};
use crate::storage::{OrgBranding, orgs as orgs_store};
use crate::web::CurrentUser;
use crate::web::views::public_status::LOGO_ROUTE;

use super::orgs::require_owner;

// ── DTOs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct StatusPageSettings {
    pub public_status_enabled: bool,
    /// Resolved header name: `public_display_name` if set, else the org name.
    pub display_name: String,
    /// Raw operator-set override (drives the form field; `null` = "use org name").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_brand_color: Option<String>,
    /// Resolved against the configured default when the operator hasn't chosen.
    pub show_powered_by: bool,
    /// Versioned logo URL on the public surface, or `null` when no logo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Where the live page is reachable, or `null` when no public surface is
    /// mounted (drives the "view / preview" link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_url: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateStatusPageRequest {
    /// Accepts a real JSON bool (API clients) or an HTML-checkbox encoding
    /// (the settings form posts via htmx `json-enc`, which sends a checked
    /// box as the string `"on"` and omits an unchecked one). Absent = off.
    #[serde(default, deserialize_with = "de_checkbox_bool")]
    #[schema(value_type = bool)]
    pub public_status_enabled: bool,
    /// Empty/blank is normalised to `null` (fall back to the org name).
    #[serde(default)]
    pub public_display_name: Option<String>,
    #[serde(default)]
    pub public_about: Option<String>,
    #[serde(default)]
    pub public_brand_color: Option<String>,
    #[serde(default, deserialize_with = "de_checkbox_bool")]
    #[schema(value_type = bool)]
    pub public_show_powered_by: bool,
}

/// Deserialize a checkbox-or-bool field. A JSON `true`/`false` passes through
/// unchanged; a string is truthy for the conventional HTML/htmx encodings
/// (`"on"`, `"true"`, `"1"`, `"yes"`); a number is truthy when non-zero. Any
/// other value — `null`, array, object, an unrecognised string — is `false`,
/// so a present-but-empty field never 422s. Field *absence* is handled by
/// `#[serde(default)]` (an unchecked checkbox is simply omitted).
fn de_checkbox_bool<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "1" | "yes"
            )
        }
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => false,
    })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LogoResponse {
    pub logo_url: String,
}

// ── Handlers ────────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/orgs/{id}/status-page",
    tag = "orgs",
    summary = "Read an org's public status-page settings (owner-only)",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = StatusPageSettings),
        (status = 401, body = ApiError),
        (status = 403, body = ApiError),
        (status = 404, body = ApiError),
    ),
)]
pub async fn get_settings(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<StatusPageSettings>> {
    let pool = state.require_db()?;
    let org = OrgId(id);
    require_owner(pool, user, org).await?;
    let ob = load_for_settings(pool, org).await?;
    Ok(Json(build_settings(&state, &ob)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/orgs/{id}/status-page",
    tag = "orgs",
    summary = "Update an org's public status-page settings (owner-only)",
    description = "Replaces every branding field from the request; the logo \
                   has its own endpoints. Toggling `public_status_enabled` off \
                   drops any cached page so the org stops serving immediately.",
    params(("id" = Uuid, Path)),
    request_body = UpdateStatusPageRequest,
    responses(
        (status = 200, body = StatusPageSettings),
        (status = 401, body = ApiError),
        (status = 403, body = ApiError),
        (status = 404, body = ApiError),
        (status = 400, body = ApiError, description = "Branding failed validation; \
                                                       `field` names the bad input"),
    ),
)]
pub async fn update_settings(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateStatusPageRequest>,
) -> Result<Json<StatusPageSettings>> {
    let pool = state.require_db()?;
    let org = OrgId(id);
    require_owner(pool, user, org).await?;
    let current = load_for_settings(pool, org).await?;
    let was_enabled = current.branding.public_status_enabled;

    // `update_public_branding` deliberately leaves `public_logo_path` alone
    // (the logo has its own endpoints), so it doesn't need to round-trip here.
    let branding = PublicOrgBranding {
        public_status_enabled: req.public_status_enabled,
        public_display_name: normalise_opt(req.public_display_name),
        public_about: normalise_opt(req.public_about),
        public_brand_color: normalise_opt(req.public_brand_color).map(|c| c.to_ascii_lowercase()),
        public_logo_path: None,
        public_show_powered_by: Some(req.public_show_powered_by),
    };
    branding.validate().map_err(|e| {
        AppError::bad_request_field(codes::BRANDING_INVALID, e.to_string(), e.field())
    })?;

    if !orgs_store::update_public_branding(pool, org, user, &branding).await? {
        return Err(AppError::not_found(
            codes::ORG_NOT_FOUND,
            "organisation not found",
        ));
    }

    // Belt-and-braces: the extractor's `public_status_enabled = true` filter
    // is the authoritative gate, but a stale cache can outlive a flip in
    // either direction. Disable→enable: an in-flight compute that started
    // before the disable can write its pre-disable snapshot after the
    // post-disable invalidate (single-flight via `try_get_with` has no
    // post-completion recheck), so without a second invalidate on re-enable
    // the next request serves the orphan snapshot for up to `cache_ttl_secs`.
    // Enable→disable: the cache must drop so the next re-enable starts fresh.
    // Drop on either transition; per-PATCH cost is negligible.
    if was_enabled != req.public_status_enabled {
        state.public_source.invalidate(org).await;
    }

    let ob = OrgBranding {
        name: current.name,
        slug: current.slug,
        branding: PublicOrgBranding {
            // Reflect what was just persisted; the logo is unchanged.
            public_logo_path: current.branding.public_logo_path,
            ..branding
        },
    };
    Ok(Json(build_settings(&state, &ob)))
}

#[utoipa::path(
    post,
    path = "/api/v1/orgs/{id}/status-page/logo",
    tag = "orgs",
    summary = "Upload a status-page logo (owner-only, multipart)",
    description = "Field `file`: PNG/JPEG/WebP. The format is sniffed from the \
                   bytes (the declared content-type is not trusted); images \
                   larger than the configured max dimension are downscaled.",
    params(("id" = Uuid, Path)),
    request_body(content = String, content_type = "multipart/form-data"),
    responses(
        (status = 200, body = LogoResponse),
        (status = 400, body = ApiError, description = "Missing file / not an allowed image"),
        (status = 401, body = ApiError),
        (status = 403, body = ApiError),
        (status = 404, body = ApiError),
        (status = 413, body = ApiError, description = "File exceeds the configured size limit"),
    ),
)]
pub async fn upload_logo(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Json<LogoResponse>> {
    let pool = state.require_db()?;
    let org = OrgId(id);
    require_owner(pool, user, org).await?;

    let raw = read_logo_field(&mut multipart).await?;
    let cfg = &state.cfg.public_status;
    if raw.len() > cfg.max_logo_size_bytes as usize {
        return Err(AppError::payload_too_large(
            codes::LOGO_TOO_LARGE,
            format!("logo exceeds {} bytes", cfg.max_logo_size_bytes),
        ));
    }
    let (mime, bytes) = process_logo(&raw, cfg.max_logo_dimension_px)?;
    // Re-check post-processing: a downscaled re-encode (notably lossless WebP)
    // can come out larger than the in-bounds original, so the pre-decode
    // check above isn't sufficient on its own.
    if bytes.len() > cfg.max_logo_size_bytes as usize {
        return Err(AppError::payload_too_large(
            codes::LOGO_TOO_LARGE,
            format!(
                "logo exceeds {} bytes after processing",
                cfg.max_logo_size_bytes
            ),
        ));
    }

    let store = LocalDiskLogoStorage::new(&cfg.logo_dir);
    let name = store
        .put(org, mime.as_content_type(), &bytes)
        .await
        .map_err(AppError::Other)?;

    let prev = orgs_store::set_public_logo_path(pool, org, user, Some(&name)).await?;
    if let Some(old) = prev.filter(|p| p != &name) {
        // Best-effort: a leftover file is harmless (unreferenced, hash-named).
        let _ = store.delete(&old).await;
    }

    let ob = load_for_settings(pool, org).await?;
    let base = public_base(&state, &ob.slug);
    let url = logo_url(base.as_deref(), &name).unwrap_or_else(|| format!("{LOGO_ROUTE}?v={name}"));
    Ok(Json(LogoResponse { logo_url: url }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/orgs/{id}/status-page/logo",
    tag = "orgs",
    summary = "Remove an org's status-page logo (owner-only)",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Removed (idempotent)"),
        (status = 401, body = ApiError),
        (status = 403, body = ApiError),
        (status = 404, body = ApiError),
    ),
)]
pub async fn delete_logo(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let pool = state.require_db()?;
    let org = OrgId(id);
    require_owner(pool, user, org).await?;
    if let Some(old) = orgs_store::set_public_logo_path(pool, org, user, None).await? {
        let store = LocalDiskLogoStorage::new(&state.cfg.public_status.logo_dir);
        let _ = store.delete(&old).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Owner-gated load of everything the GET / post-mutation responses need:
/// branding + org name (header fallback) + slug (live URL) — one round-trip.
pub(crate) async fn load_for_settings(pool: &sqlx::PgPool, org: OrgId) -> Result<OrgBranding> {
    orgs_store::load_public_branding(pool, org)
        .await?
        .ok_or_else(|| AppError::not_found(codes::ORG_NOT_FOUND, "organisation not found"))
}

pub(crate) fn build_settings(state: &AppState, ob: &OrgBranding) -> StatusPageSettings {
    let cfg = &state.cfg.public_status;
    let b = &ob.branding;
    let base = public_base(state, &ob.slug);
    StatusPageSettings {
        public_status_enabled: b.public_status_enabled,
        display_name: ob.resolved_display_name().to_owned(),
        public_display_name: b.public_display_name.clone(),
        public_about: b.public_about.clone(),
        public_brand_color: b.public_brand_color.clone(),
        show_powered_by: b.show_powered_by(cfg.default_show_powered_by),
        logo_url: b
            .public_logo_path
            .as_deref()
            .and_then(|p| logo_url(base.as_deref(), p)),
        status_url: base.as_ref().map(|origin| status_url(state, origin)),
    }
}

/// Public URL shown to the operator. SaaS subdomain mode serves the page at
/// the apex of `{slug}.{base_domain}` (industry parity); the path-based
/// self-host surface keeps `/status` as the mount point.
fn status_url(state: &AppState, origin: &str) -> String {
    if subdomain_public_routes_enabled(&state.cfg) {
        // origin is already `https://{slug}.{base_domain}` — apex.
        origin.to_owned()
    } else {
        format!("{origin}/status")
    }
}

/// Public logo URL for `path`, or `None` when no public surface is mounted.
/// One builder so the operator preview and the upload response can't disagree
/// on shape (origin-prefixed in subdomain mode, host-relative in path mode).
fn logo_url(base: Option<&str>, path: &str) -> Option<String> {
    base.map(|origin| format!("{origin}{LOGO_ROUTE}?v={path}"))
}

/// Origin the public page is served from: an absolute `https://…` host in
/// subdomain (SaaS) mode, an empty string (same operator host) in path mode,
/// or `None` when no public surface is mounted.
fn public_base(state: &AppState, slug: &str) -> Option<String> {
    if subdomain_public_routes_enabled(&state.cfg) {
        return Some(format!(
            "https://{slug}.{}",
            state.cfg.public_status.base_domain
        ));
    }
    if path_based_public_routes_enabled(&state.cfg) {
        return Some(String::new());
    }
    None
}

fn normalise_opt(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Pulls the first `file` part out of the multipart body.
async fn read_logo_field(multipart: &mut Multipart) -> Result<Vec<u8>> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(codes::LOGO_MISSING, e.to_string()))?
    {
        if field.name() == Some("file") {
            return field
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| AppError::bad_request(codes::LOGO_MISSING, e.to_string()));
        }
    }
    Err(AppError::bad_request(
        codes::LOGO_MISSING,
        "expected a multipart field named `file`",
    ))
}

/// Validates the image by *sniffing the bytes* (the client-declared
/// content-type is ignored — that's what stops an HTML/SVG payload being
/// stored and later served with an image content-type), then downscales it to
/// fit `max_dim` if either side is larger. Bytes are re-encoded only when a
/// resize actually happened, so an in-bounds upload is stored verbatim.
fn process_logo(raw: &[u8], max_dim: u32) -> Result<(LogoMime, Vec<u8>)> {
    let fmt = image::guess_format(raw).map_err(|_| {
        AppError::bad_request(codes::LOGO_TYPE_INVALID, "unrecognised image format")
    })?;
    let mime = LogoMime::from_image_format(fmt).ok_or_else(|| {
        AppError::bad_request(codes::LOGO_TYPE_INVALID, "logo must be PNG, JPEG, or WebP")
    })?;
    // Decompression-bomb guard: bound the decoder's allocation and the
    // canvas dimensions *before* the full decode. `load_from_memory` would
    // happily allocate gigabytes for a tiny file that declares a 60000×60000
    // canvas. The hard pixel ceiling is generous (real source art is far
    // smaller, and oversized-but-honest images are downscaled below); the
    // alloc cap is the actual DoS backstop. 10_000² × 4 B/px ≈ 400 MB >
    // 128 MiB, so for any normal aspect ratio `max_alloc` is the binding
    // gate; the pixel ceilings only catch pathological strips (e.g.
    // 10001×1) — they are not independently tunable.
    const HARD_DIM_PX: u32 = 10_000;
    const MAX_DECODE_ALLOC: u64 = 128 * 1024 * 1024;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(HARD_DIM_PX);
    limits.max_image_height = Some(HARD_DIM_PX);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(raw), fmt);
    reader.limits(limits);
    let img = reader.decode().map_err(|_| {
        AppError::bad_request(
            codes::LOGO_DECODE_FAILED,
            "could not decode image — malformed, or its dimensions exceed the limit",
        )
    })?;
    if img.width() <= max_dim && img.height() <= max_dim {
        return Ok((mime, raw.to_vec()));
    }
    let resized = img.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    resized.write_to(&mut out, fmt).map_err(|_| {
        AppError::bad_request(codes::LOGO_DECODE_FAILED, "could not re-encode image")
    })?;
    Ok((mime, out.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_blanks_to_none() {
        assert_eq!(normalise_opt(Some("  ".into())), None);
        assert_eq!(normalise_opt(Some(String::new())), None);
        assert_eq!(normalise_opt(None), None);
        assert_eq!(normalise_opt(Some("  hi ".into())), Some("hi".into()));
    }

    #[test]
    fn update_request_accepts_json_bool_and_html_checkbox() {
        // API client: real JSON booleans round-trip unchanged.
        let r: UpdateStatusPageRequest = serde_json::from_str(
            r#"{"public_status_enabled":true,"public_show_powered_by":false}"#,
        )
        .unwrap();
        assert!(r.public_status_enabled);
        assert!(!r.public_show_powered_by);

        // htmx json-enc: a checked box arrives as the string "on".
        let r: UpdateStatusPageRequest =
            serde_json::from_str(r#"{"public_status_enabled":"on"}"#).unwrap();
        assert!(r.public_status_enabled);
        // An unchecked box is omitted entirely → default false (not a 422).
        assert!(!r.public_show_powered_by);

        // Both fields omitted (was a required field before the fix) → false,
        // never a deserialize 422.
        let r: UpdateStatusPageRequest = serde_json::from_str("{}").unwrap();
        assert!(!r.public_status_enabled);
        assert!(!r.public_show_powered_by);

        // Explicit string falsey values, numbers, and null all coerce to
        // false rather than erroring.
        let r: UpdateStatusPageRequest = serde_json::from_str(
            r#"{"public_status_enabled":"false","public_show_powered_by":""}"#,
        )
        .unwrap();
        assert!(!r.public_status_enabled);
        assert!(!r.public_show_powered_by);

        let r: UpdateStatusPageRequest =
            serde_json::from_str(r#"{"public_status_enabled":1,"public_show_powered_by":null}"#)
                .unwrap();
        assert!(r.public_status_enabled);
        assert!(!r.public_show_powered_by);
    }

    #[test]
    fn process_logo_rejects_non_image() {
        let err = process_logo(b"<svg xmlns='http://www.w3.org/2000/svg'/>", 1200).unwrap_err();
        assert!(matches!(err, AppError::BadRequest { .. }));
    }

    #[test]
    fn process_logo_rejects_oversized_canvas() {
        // Decompression-bomb shape: cheap to encode (1px tall) but a width
        // past the hard pixel ceiling. The decoder's `Limits` must reject it
        // *before* materialising the canvas, not OOM.
        let wide = image::RgbaImage::from_pixel(10_001, 1, image::Rgba([0, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(wide)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let err = process_logo(&buf.into_inner(), 1200).unwrap_err();
        assert!(
            matches!(&err, AppError::BadRequest { code, .. } if *code == codes::LOGO_DECODE_FAILED),
            "expected LOGO_DECODE_FAILED, got {err:?}"
        );
    }

    #[test]
    fn process_logo_passes_small_png_through_untouched() {
        // 1x1 PNG.
        let png = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(png)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let raw = buf.into_inner();
        let (mime, out) = process_logo(&raw, 1200).unwrap();
        assert_eq!(mime, LogoMime::Png);
        assert_eq!(out, raw, "in-bounds image must be stored verbatim");
    }

    #[test]
    fn process_logo_downscales_oversized() {
        let big = image::RgbaImage::from_pixel(2000, 1000, image::Rgba([9, 9, 9, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(big)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let (_, out) = process_logo(&buf.into_inner(), 512).unwrap();
        let decoded = image::load_from_memory(&out).unwrap();
        assert!(decoded.width() <= 512 && decoded.height() <= 512);
    }
}
