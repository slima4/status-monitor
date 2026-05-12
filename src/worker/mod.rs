pub mod circuit_breaker;
pub mod http_check;
pub mod pool;
pub mod tcp_check;

pub use http_check::execute_http_check;
pub use pool::{CheckTask, WorkerPool};

use uuid::Uuid;

use crate::domain::{CheckResult, CheckSpec};

pub async fn execute(
    target_id: Uuid,
    spec: &CheckSpec,
    http_client: &reqwest::Client,
) -> CheckResult {
    match spec {
        CheckSpec::Http(http) => execute_http_check(target_id, http, http_client).await,
        CheckSpec::Tcp(tcp) => tcp_check::execute_tcp_check(target_id, tcp).await,
    }
}
