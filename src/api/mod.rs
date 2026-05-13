pub mod docs;
pub mod error;
pub mod handlers;
pub mod idempotency;
pub mod middleware;
pub mod page;
pub mod public_error;
pub mod redaction;
pub mod routes;
pub mod types;

pub use docs::ApiDoc;
pub use error::{ApiError, ApiErrorBody, codes};
pub use idempotency::{IdempotencyCache, spawn_pruner as spawn_idempotency_pruner};
pub use page::{
    PageEnvelope, PageOfCheckResult, PageOfIncident, PageOfPublicIncident, PageOfTagCount,
    PageOfTarget,
};
pub use public_error::{PublicApiError, PublicApiErrorBody, PublicAppError};
pub use routes::build_router;
pub use types::{
    BulkAction, BulkActionFailure, BulkActionRequest, BulkActionResponse, DashboardSummary,
    Last24hSummary, StatusBreakdown, SystemSummary, TagCount, TargetsSummary, TestRequest,
    TestResponse,
};
