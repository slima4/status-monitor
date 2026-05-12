pub mod circuit_breaker;
pub mod http_check;
pub mod pool;
pub mod tcp_check;

pub use http_check::execute_http_check;
pub use pool::{CheckTask, WorkerPool};

use uuid::Uuid;

use crate::domain::{CheckResult, CheckSpec};
use crate::http_client::HttpClients;

pub async fn execute(target_id: Uuid, spec: &CheckSpec, clients: &HttpClients) -> CheckResult {
    match spec {
        CheckSpec::Http(http) => execute_http_check(target_id, http, clients).await,
        CheckSpec::Tcp(tcp) => tcp_check::execute_tcp_check(target_id, tcp).await,
    }
}
