//! Single source of truth for assembling the merged app router
//! (API + web UI) with the cross-cutting layers applied. `main.rs`
//! and every test-harness call site routes through here so a future
//! site can't silently miss CSRF or tenant-host isolation.
//!
//! Layer order (outermost first, runs earliest on request):
//!   1. tenant-host isolation — 404s operator surface on tenant hosts
//!   2. CSRF — rejects state-changing requests without the custom header
//!
//! CSRF wraps the *merged* router so any future state-changing route
//! added to `web::routes` is protected without a separate wiring step.

use axum::Router;
use axum::middleware::from_fn_with_state;
use tokio_util::sync::CancellationToken;

use crate::app::AppState;
use crate::{api, web};

/// Build the full app router (API + web UI) with the cross-cutting
/// guards applied. `main.rs` and the test harness both route through
/// this function so a future call site can't silently miss either
/// guard.
pub fn build_app_router(state: AppState, shutdown: CancellationToken) -> Router {
    let merged = api::build_router(state.clone(), shutdown).merge(web::routes(state.clone()));
    apply_cross_cutting_layers(merged, state)
}

/// API-only variant for tests that need to exercise the JSON surface
/// without the HTML routes. Same cross-cutting layers apply so the
/// test surface mirrors production gating exactly.
pub fn build_app_router_api_only(state: AppState, shutdown: CancellationToken) -> Router {
    apply_cross_cutting_layers(api::build_router(state.clone(), shutdown), state)
}

fn apply_cross_cutting_layers(router: Router, state: AppState) -> Router {
    // Last `.layer()` is OUTERMOST in axum — `tenant_host_isolation`
    // runs first, then CSRF. Keep this order: 404 a tenant-host
    // operator route before bothering with the CSRF constant-time
    // header compare. Reordering reverses request semantics.
    router
        .layer(from_fn_with_state(
            state.clone(),
            web::auth::csrf::middleware,
        ))
        .layer(from_fn_with_state(state, web::host::tenant_host_isolation))
}
