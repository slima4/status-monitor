use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::{Value, json};
use status_monitor::domain::{CheckStatus, DomainExpiryCheck};
use status_monitor::http_outbound::build_outbound_client;
use status_monitor::worker::domain_expiry::run_check;
use status_monitor::worker::rdap::RdapClient;

#[derive(Clone)]
struct ServerState {
    base_url: String,
    expiration: Arc<Mutex<chrono::DateTime<Utc>>>,
    registrar: Option<String>,
    fail_lookup: bool,
}

async fn handle_bootstrap(State(state): State<ServerState>) -> axum::Json<Value> {
    axum::Json(json!({
        "version": "1.0",
        "publication": "2026-01-01T00:00:00Z",
        "services": [[["example"], [format!("{}/", state.base_url)]]]
    }))
}

async fn handle_domain(
    State(state): State<ServerState>,
    Path(_domain): Path<String>,
) -> (StatusCode, axum::Json<Value>) {
    if state.fail_lookup {
        return (StatusCode::NOT_FOUND, axum::Json(json!({})));
    }
    let exp = *state.expiration.lock();
    let entities = match &state.registrar {
        Some(name) => json!([{
            "objectClassName": "entity",
            "roles": ["registrar"],
            "vcardArray": ["vcard", [
                ["version", {}, "text", "4.0"],
                ["fn", {}, "text", name]
            ]]
        }]),
        None => json!([]),
    };
    let body = json!({
        "events": [
            {"eventAction": "registration", "eventDate": "2020-01-01T00:00:00Z"},
            {"eventAction": "expiration", "eventDate": exp.to_rfc3339()}
        ],
        "entities": entities
    });
    (StatusCode::OK, axum::Json(body))
}

async fn spawn_rdap_fixture(
    expiration: chrono::DateTime<Utc>,
    registrar: Option<&str>,
    fail_lookup: bool,
) -> (SocketAddr, ServerState) {
    // Bind before constructing state so the bootstrap response can advertise
    // the same address we'll serve from. Can't delegate to `common::spawn_router`
    // here — it binds internally, but the fixture's bootstrap payload needs the
    // addr baked into state at construction time.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = ServerState {
        base_url: format!("http://{addr}"),
        expiration: Arc::new(Mutex::new(expiration)),
        registrar: registrar.map(str::to_owned),
        fail_lookup,
    };
    let app = Router::new()
        .route("/bootstrap.json", get(handle_bootstrap))
        .route("/domain/{domain}", get(handle_domain))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

fn make_check(domain: &str, warn: u32, critical: u32) -> DomainExpiryCheck {
    DomainExpiryCheck {
        domain: domain.into(),
        warn_days: warn,
        critical_days: critical,
        timeout: Duration::from_secs(5),
    }
}

fn client_for(addr: SocketAddr) -> RdapClient {
    RdapClient::with_bootstrap_url(
        build_outbound_client(status_monitor::security::SsrfGuard::relaxed_for_tests()),
        format!("http://{addr}/bootstrap.json"),
    )
}

#[tokio::test]
async fn domain_expiry_up_when_far_from_expiry() {
    let (addr, _) = spawn_rdap_fixture(
        Utc::now() + chrono::Duration::days(120),
        Some("Acme"),
        false,
    )
    .await;
    let verdict = run_check(&make_check("foo.example", 30, 7), &client_for(addr))
        .await
        .expect("rdap ok");
    assert_eq!(verdict.status, CheckStatus::Up);
}

#[tokio::test]
async fn domain_expiry_degraded_under_warn_days() {
    let (addr, _) =
        spawn_rdap_fixture(Utc::now() + chrono::Duration::days(20), Some("Acme"), false).await;
    let verdict = run_check(&make_check("foo.example", 30, 7), &client_for(addr))
        .await
        .expect("rdap ok");
    assert_eq!(verdict.status, CheckStatus::Degraded);
    let body: Value = serde_json::from_str(&verdict.details_json).unwrap();
    assert_eq!(body["registrar"], "Acme");
    assert!(body["days_remaining"].as_i64().unwrap() < 30);
}

#[tokio::test]
async fn domain_expiry_down_under_critical_days() {
    let (addr, _) = spawn_rdap_fixture(Utc::now() + chrono::Duration::days(3), None, false).await;
    let verdict = run_check(&make_check("foo.example", 30, 7), &client_for(addr))
        .await
        .expect("rdap ok");
    assert_eq!(verdict.status, CheckStatus::Down);
}

#[tokio::test]
async fn domain_expiry_down_when_expired() {
    let (addr, _) = spawn_rdap_fixture(Utc::now() - chrono::Duration::days(1), None, false).await;
    let verdict = run_check(&make_check("foo.example", 30, 7), &client_for(addr))
        .await
        .expect("rdap ok");
    assert_eq!(verdict.status, CheckStatus::Down);
    let body: Value = serde_json::from_str(&verdict.details_json).unwrap();
    assert!(body["days_remaining"].as_i64().unwrap() < 0);
}

#[tokio::test]
async fn domain_expiry_errors_on_unknown_tld() {
    let (addr, _) = spawn_rdap_fixture(Utc::now() + chrono::Duration::days(100), None, false).await;
    let err = run_check(&make_check("foo.unknowntld", 30, 7), &client_for(addr))
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("rdap"));
}

#[tokio::test]
async fn domain_expiry_errors_on_rdap_404() {
    let (addr, _) = spawn_rdap_fixture(Utc::now() + chrono::Duration::days(100), None, true).await;
    let err = run_check(&make_check("foo.example", 30, 7), &client_for(addr))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("404"));
}
