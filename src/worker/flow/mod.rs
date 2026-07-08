//! Browser-driven flow monitor: replays a fixed step sequence (login /
//! transaction) against a real page through a headless CDP engine, verifying a
//! login *session* rather than a single request. Runs on any flow-capable node
//! (the control plane or a flow-capable agent).

pub mod engine;
mod executor;

use chrono::Utc;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, FlowCheck};
use engine::CdpEngine;
use executor::RunResult;

pub async fn execute_flow_check(
    target_id: Uuid,
    org_id: Uuid,
    flow: &FlowCheck,
    engine: Option<&CdpEngine>,
) -> CheckResult {
    let Some(engine) = engine else {
        return CheckResult::error(target_id, org_id, "flow engine not configured on this node");
    };

    let started = Utc::now();
    // `run` applies the flow deadline internally, after it holds a concurrency
    // slot, and returns the elapsed probe time excluding any queue wait.
    let (run, elapsed) = engine.run(flow).await;

    let (status, error) = match run {
        RunResult::Passed => (CheckStatus::Up, None),
        RunResult::Failed { step, op, reason } => (
            CheckStatus::Down,
            Some(format!(
                "step {}/{} {op}: {reason}",
                step + 1,
                flow.steps.len()
            )),
        ),
        RunResult::Engine(e) => (CheckStatus::Error, Some(e)),
    };

    CheckResult {
        target_id,
        org_id,
        timestamp: started,
        status,
        duration_ms: elapsed,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: None,
        response_code: None,
        response_size: None,
        error,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::domain::FlowStep;
    use engine::{CdpEngine, FlowEngineConfig};

    #[tokio::test]
    async fn missing_engine_reports_error_not_panic() {
        let flow = FlowCheck {
            start_url: url::Url::parse("https://example.com/login").unwrap(),
            steps: vec![FlowStep::AssertUrl {
                contains: "/x".into(),
            }],
            timeout: Duration::from_secs(5),
            step_timeout: Duration::from_secs(1),
            verify_tls: true,
        };
        let r = execute_flow_check(Uuid::nil(), Uuid::nil(), &flow, None).await;
        assert_eq!(r.status, CheckStatus::Error);
        assert!(r.error.unwrap().contains("not configured"));
    }

    // End-to-end through the real engine + a spawned Lightpanda. Ignored: needs
    // the engine binary (path in FLOW_LIGHTPANDA_BIN) and outbound network.
    #[tokio::test]
    #[ignore]
    async fn real_login_flow_passes() {
        let Ok(bin) = std::env::var("FLOW_LIGHTPANDA_BIN") else {
            eprintln!("FLOW_LIGHTPANDA_BIN unset; skipping");
            return;
        };
        let engine = CdpEngine::new(FlowEngineConfig {
            binary: bin.into(),
            max_concurrency: 1,
            mem_limit_bytes: 0,
            block_private_networks: true,
            block_cidrs: "169.254.0.0/16,127.0.0.0/8".into(),
            v8_max_heap_mb: 0,
            max_response_bytes: 0,
            user_agent_suffix: String::new(),
        });
        let flow = FlowCheck {
            start_url: url::Url::parse("https://the-internet.herokuapp.com/login").unwrap(),
            steps: vec![
                FlowStep::Fill {
                    selector: "#username".into(),
                    value: "tomsmith".into(),
                },
                FlowStep::Fill {
                    selector: "#password".into(),
                    value: "SuperSecretPassword!".into(),
                },
                FlowStep::Click {
                    selector: "button[type=\"submit\"]".into(),
                },
                FlowStep::AssertUrl {
                    contains: "/secure".into(),
                },
                FlowStep::AssertText {
                    selector: None,
                    contains: "secure area".into(),
                },
            ],
            timeout: Duration::from_secs(30),
            step_timeout: Duration::from_secs(10),
            verify_tls: true,
        };
        let r = execute_flow_check(Uuid::nil(), Uuid::nil(), &flow, Some(&engine)).await;
        assert_eq!(r.status, CheckStatus::Up, "error={:?}", r.error);
    }

    fn sandbox_engine(bin: &str, block: bool) -> CdpEngine {
        CdpEngine::new(FlowEngineConfig {
            binary: bin.into(),
            max_concurrency: 1,
            mem_limit_bytes: 0,
            block_private_networks: block,
            block_cidrs: if block {
                "127.0.0.0/8".into()
            } else {
                String::new()
            },
            v8_max_heap_mb: 0,
            max_response_bytes: 0,
            user_agent_suffix: String::new(),
        })
    }

    async fn run_loopback_flow(engine: &CdpEngine, url: &str) -> CheckResult {
        let flow = FlowCheck {
            start_url: url::Url::parse(url).unwrap(),
            steps: vec![FlowStep::AssertText {
                selector: None,
                contains: "flowmarker".into(),
            }],
            timeout: Duration::from_secs(15),
            step_timeout: Duration::from_secs(5),
            verify_tls: false,
        };
        execute_flow_check(Uuid::nil(), Uuid::nil(), &flow, Some(engine)).await
    }

    // Causal proof the egress sandbox works: the same loopback page the browser
    // reaches with blocking off is unreachable with it on. A private target that
    // merely refused the connection would fail either way; contrasting the two
    // runs pins the failure on the block, not the network.
    #[tokio::test]
    #[ignore]
    async fn egress_sandbox_blocks_loopback_but_not_public() {
        use tokio::io::AsyncWriteExt;

        let Ok(bin) = std::env::var("FLOW_LIGHTPANDA_BIN") else {
            eprintln!("FLOW_LIGHTPANDA_BIN unset; skipping");
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let _ = sock.readable().await;
                let _ = sock.try_read(&mut buf);
                let body = "<html><body>flowmarker</body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let url = format!("http://{addr}/");

        let reached = run_loopback_flow(&sandbox_engine(&bin, false), &url).await;
        assert_eq!(
            reached.status,
            CheckStatus::Up,
            "loopback reachable without the block; error={:?}",
            reached.error
        );

        let blocked = run_loopback_flow(&sandbox_engine(&bin, true), &url).await;
        assert_ne!(
            blocked.status,
            CheckStatus::Up,
            "loopback must be unreachable with the egress sandbox on"
        );

        server.abort();
    }
}
