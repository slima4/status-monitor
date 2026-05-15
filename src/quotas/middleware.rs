//! Per-org / per-user rate-limit middleware.
//!
//! Runs *after* the API-token auth middleware (so `AuthContext` is present
//! for the Bearer path) and resolves the subject via the same `CurrentUser`
//! / `CurrentOrg` extractors the handlers use — no duplicated auth logic and
//! no TCP-peer keying. Unauthenticated requests fall through untouched;
//! Caddy applies the per-IP tier to those (§4.3).

use axum::extract::{FromRequestParts, Request, State};
use axum::http::{Method, request::Parts};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
use crate::quotas::ratelimit::{RateLimitCategory, RateLimitKey};
use crate::quotas::service::record_quota_event;
use crate::web::auth::{CurrentOrg, CurrentUser};

fn categorize(parts: &Parts) -> RateLimitCategory {
    let path = parts.uri.path();
    if path.contains("/bulk") {
        return RateLimitCategory::BulkOps;
    }
    if path.ends_with("/test") {
        return RateLimitCategory::TestNow;
    }
    if path.ends_with("/check-now") {
        return RateLimitCategory::CheckNow;
    }
    match parts.method {
        Method::GET | Method::HEAD | Method::OPTIONS => RateLimitCategory::ApiReads,
        _ => RateLimitCategory::ApiWrites,
    }
}

pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = req.into_parts();

    // Resolve the authenticated subject with the real extractors. A failure
    // means unauthenticated (or no org) — pass through; Caddy rate-limits
    // those per-IP. Never reject here.
    let user = CurrentUser::from_request_parts(&mut parts, &state)
        .await
        .ok()
        .map(|CurrentUser(u)| u);
    let org = CurrentOrg::from_request_parts(&mut parts, &state)
        .await
        .ok()
        .map(|CurrentOrg(o)| o);

    if user.is_none() && org.is_none() {
        return next.run(Request::from_parts(parts, body)).await;
    }

    let category = categorize(&parts);

    // Plan drives the limits. Resolve via the org when known; fall back to
    // the user's effective plan otherwise. A resolution error must not block
    // traffic — degrade to "no app-side limit" and let Caddy hold the line.
    let plan = match org {
        Some(o) => state.quotas.limit_for_org(o).await.ok(),
        None => None,
    };
    let Some(plan) = plan else {
        return next.run(Request::from_parts(parts, body)).await;
    };

    // Per-org first (protects shared resources), then per-user. Whichever is
    // hit first produces the 429.
    if let Some(o) = org
        && let Err(d) = state
            .rate_limits
            .check(RateLimitKey::Org(o, category), "per_org", &plan)
    {
        record_quota_event(
            state.db.clone(),
            Some(o),
            user,
            "rate_limited",
            None,
            serde_json::json!({ "scope": d.scope }),
            None,
        );
        return crate::error::AppError::RateLimited {
            scope: d.scope,
            retry_after_secs: d.retry_after_secs,
        }
        .into_response();
    }
    if let Some(u) = user
        && let Err(d) = state
            .rate_limits
            .check(RateLimitKey::User(u, category), "per_user", &plan)
    {
        record_quota_event(
            state.db.clone(),
            org,
            Some(u),
            "rate_limited",
            None,
            serde_json::json!({ "scope": d.scope }),
            None,
        );
        return crate::error::AppError::RateLimited {
            scope: d.scope,
            retry_after_secs: d.retry_after_secs,
        }
        .into_response();
    }

    next.run(Request::from_parts(parts, body)).await
}
