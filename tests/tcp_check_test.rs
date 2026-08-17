//! The TCP check's failure text, which reaches the customer as their reason.

mod common;

use std::time::Duration;

use uptimepage::domain::{CheckStatus, TcpCheck};
use uptimepage::worker::tcp_check::execute_tcp_check;
use uuid::Uuid;

/// The raw `io::Error` reads "Connection refused (os error 61)" here and 111 on
/// Linux, so asserting the exact text is what pins the normalisation.
///
/// Port 1 rather than a bound-then-dropped ephemeral one: the ephemeral port is
/// unowned between the drop and the connect, so a concurrent bind takes it and
/// the connect succeeds instead of being refused. Nothing listens on tcpmux,
/// and reaching a privileged port needs no privilege.
#[tokio::test]
async fn a_refused_connection_reads_the_same_as_it_does_over_http() {
    let check = TcpCheck {
        host: "127.0.0.1".into(),
        port: 1,
        timeout: Duration::from_secs(3),
    };

    let result =
        execute_tcp_check(Uuid::now_v7(), Uuid::nil(), &check, &common::test_client()).await;

    assert_eq!(result.status, CheckStatus::Down);
    let error = result.error.expect("a refused connect carries a reason");
    assert_eq!(error, "connection refused");
}
