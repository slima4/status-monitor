//! Single source of truth for assembling the merged app router
//! (API + web UI) with tenant-host isolation applied. `main.rs` and
//! every test-harness call site routes through here so a future site
//! can't silently miss the isolation middleware.

use axum::Router;
use tokio_util::sync::CancellationToken;

use crate::app::AppState;
use crate::{api, web};

/// Build the full app router (API + web UI) with tenant-host isolation
/// applied. `tenant_host_isolation` must wrap the merged router so the
/// API surface is gated the same way the web UI is.
pub fn build_app_router(state: AppState, shutdown: CancellationToken) -> Router {
    let merged = api::build_router(state.clone(), shutdown).merge(web::routes(state.clone()));
    apply_tenant_isolation(merged, state)
}

/// API-only variant for tests that need to exercise the JSON surface
/// without the HTML routes. Isolation still applies so a tenant-host
/// request to `/api/v1/*` 404s the same way it does in production.
pub fn build_app_router_api_only(state: AppState, shutdown: CancellationToken) -> Router {
    apply_tenant_isolation(api::build_router(state.clone(), shutdown), state)
}

fn apply_tenant_isolation(router: Router, state: AppState) -> Router {
    router.layer(axum::middleware::from_fn_with_state(
        state,
        web::host::tenant_host_isolation,
    ))
}
