use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, State};
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::{CheckSpec, ExpectedStatus, HttpMethod, Target};
use crate::error::AppError;
use crate::web::error::WebResult;

pub struct AuthFieldState {
    pub has_basic: bool,
    pub has_bearer: bool,
}

impl AuthFieldState {
    pub fn basic_initial_mode(&self) -> &'static str {
        if self.has_basic { "redacted" } else { "create" }
    }

    pub fn bearer_initial_mode(&self) -> &'static str {
        if self.has_bearer { "redacted" } else { "create" }
    }
}

pub struct HttpFields {
    pub url: String,
    pub method: &'static str,
    pub timeout_ms: u64,
    pub follow_redirects: bool,
    pub max_redirects: u8,
    pub expected_kind: &'static str,
    pub expected_exact: u16,
    pub expected_range_min: u16,
    pub expected_range_max: u16,
    pub expected_one_of_csv: String,
    pub expected_body_contains: String,
    pub headers_json: String,
    pub body: String,
    pub verify_tls: bool,
    pub auth: AuthFieldState,
}

impl Default for HttpFields {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: "GET",
            timeout_ms: 5_000,
            follow_redirects: false,
            max_redirects: 0,
            expected_kind: "exact",
            expected_exact: 200,
            expected_range_min: 200,
            expected_range_max: 299,
            expected_one_of_csv: String::new(),
            expected_body_contains: String::new(),
            headers_json: "{}".into(),
            body: String::new(),
            verify_tls: true,
            auth: AuthFieldState {
                has_basic: false,
                has_bearer: false,
            },
        }
    }
}

pub struct TcpFields {
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
}

impl Default for TcpFields {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 0,
            timeout_ms: 3_000,
        }
    }
}

pub struct FormModel {
    pub mode: &'static str,
    pub id: String,
    pub action: String,
    pub submit_method: &'static str,
    pub name: String,
    pub interval_s: u64,
    pub enabled: bool,
    pub tags_csv: String,
    pub check_type: &'static str,
    pub http: HttpFields,
    pub tcp: TcpFields,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/form.html")]
pub struct FormPage {
    pub active_tab: &'static str,
    pub form: FormModel,
}

pub async fn new_form() -> FormPage {
    FormPage {
        active_tab: "targets",
        form: FormModel {
            mode: "create",
            id: String::new(),
            action: "/api/v1/targets".into(),
            submit_method: "POST",
            name: String::new(),
            interval_s: 60,
            enabled: true,
            tags_csv: String::new(),
            check_type: "http",
            http: HttpFields::default(),
            tcp: TcpFields::default(),
        },
    }
}

pub async fn edit_form(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> WebResult<FormPage> {
    let target = state
        .target_store
        .get(id)
        .await?
        .ok_or_else(|| AppError::not_found("TARGET_NOT_FOUND", "target not found"))?;
    Ok(FormPage {
        active_tab: "targets",
        form: build_edit_form(target)?,
    })
}

fn build_edit_form(t: Target) -> Result<FormModel, AppError> {
    let tags_csv = t.tags.join(", ");
    let action = format!("/api/v1/targets/{}", t.id);
    let id = t.id.to_string();

    let (check_type, http, tcp) = match t.check {
        CheckSpec::Http(h) => ("http", http_fields_from(h), TcpFields::default()),
        CheckSpec::Tcp(c) => (
            "tcp",
            HttpFields::default(),
            TcpFields {
                host: c.host,
                port: c.port,
                timeout_ms: c.timeout.as_millis() as u64,
            },
        ),
        CheckSpec::TlsCert(_) | CheckSpec::DomainExpiry(_) => {
            return Err(AppError::unprocessable(
                "UNSUPPORTED_EDIT",
                "Editing TLS-cert and domain-expiry checks is not yet available in the UI. \
                 Use the JSON API directly.",
            ));
        }
    };

    Ok(FormModel {
        mode: "edit",
        id,
        action,
        submit_method: "PATCH",
        name: t.name,
        interval_s: t.interval.as_secs(),
        enabled: t.enabled,
        tags_csv,
        check_type,
        http,
        tcp,
    })
}

fn http_fields_from(h: crate::domain::HttpCheck) -> HttpFields {
    let has_basic = h.basic_auth.is_some();
    let has_bearer = h.bearer_token.is_some();
    let (kind, exact, range_min, range_max, one_of) = match h.expected_status {
        ExpectedStatus::Exact(c) => ("exact", c, 200, 299, String::new()),
        ExpectedStatus::Range { min, max } => ("range", 200, min, max, String::new()),
        ExpectedStatus::OneOf(v) => (
            "one_of",
            200,
            200,
            299,
            v.iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    let headers_json =
        serde_json::to_string_pretty(&h.headers).unwrap_or_else(|_| "{}".into());
    HttpFields {
        url: h.url.to_string(),
        method: http_method_str(h.method),
        timeout_ms: h.timeout.as_millis() as u64,
        follow_redirects: h.follow_redirects,
        max_redirects: h.max_redirects,
        expected_kind: kind,
        expected_exact: exact,
        expected_range_min: range_min,
        expected_range_max: range_max,
        expected_one_of_csv: one_of,
        expected_body_contains: h.expected_body_contains.unwrap_or_default(),
        headers_json,
        body: h.body.unwrap_or_default(),
        verify_tls: h.verify_tls,
        auth: AuthFieldState { has_basic, has_bearer },
    }
}

fn http_method_str(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Head => "HEAD",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::redaction::REDACTED;

    #[tokio::test]
    async fn new_form_renders_empty_create() {
        let page = new_form().await;
        let html = page.render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("New target"));
        assert!(html.contains(r#"data-action="/api/v1/targets""#));
        assert!(html.contains(r#"data-method="POST""#));
        assert!(html.contains(r#"data-mode="create""#));
        assert!(html.contains(r#"name="check_type" value="http""#));
    }

    #[test]
    fn edit_form_renders_redacted_state_for_existing_auth() {
        use crate::domain::HttpCheck;
        use std::collections::HashMap;
        use std::time::Duration;
        use url::Url;

        let t = Target {
            id: uuid::Uuid::nil(),
            name: "api".into(),
            check: CheckSpec::Http(HttpCheck {
                url: Url::parse("https://example.com").unwrap(),
                method: HttpMethod::Get,
                timeout: Duration::from_millis(5_000),
                follow_redirects: false,
                max_redirects: 0,
                expected_status: ExpectedStatus::Exact(200),
                expected_body_contains: None,
                headers: HashMap::new(),
                body: None,
                verify_tls: true,
                basic_auth: Some((REDACTED.into(), REDACTED.into())),
                bearer_token: Some(REDACTED.into()),
            }),
            interval: Duration::from_secs(60),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = build_edit_form(t).unwrap();
        assert!(form.http.auth.has_basic);
        assert!(form.http.auth.has_bearer);
        assert_eq!(form.submit_method, "PATCH");
        let page = FormPage {
            active_tab: "targets",
            form,
        };
        let html = page.render().unwrap();
        assert!(html.contains(r#"data-initial-mode="redacted""#));
        assert!(html.contains("Replace credentials"));
        assert!(html.contains("Replace token"));
    }

    #[test]
    fn edit_form_maps_tcp_target_fields() {
        use crate::domain::TcpCheck;
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "db".into(),
            check: CheckSpec::Tcp(TcpCheck {
                host: "db.example.com".into(),
                port: 5432,
                timeout: Duration::from_millis(2_500),
            }),
            interval: Duration::from_secs(30),
            enabled: false,
            tags: vec!["prod".into(), "db".into()],
            alerts: Default::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = build_edit_form(t).unwrap();
        assert_eq!(form.check_type, "tcp");
        assert_eq!(form.tcp.host, "db.example.com");
        assert_eq!(form.tcp.port, 5432);
        assert_eq!(form.tcp.timeout_ms, 2_500);
        assert_eq!(form.interval_s, 30);
        assert!(!form.enabled);
        assert_eq!(form.tags_csv, "prod, db");
        assert_eq!(form.submit_method, "PATCH");
    }

    #[test]
    fn edit_form_rejects_tls_cert_target() {
        use crate::domain::TlsCertCheck;
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "tls".into(),
            check: CheckSpec::TlsCert(TlsCertCheck {
                host: "example.com".into(),
                port: 443,
                server_name: None,
                warn_days: 30,
                critical_days: 7,
                timeout: Duration::from_secs(5),
            }),
            interval: Duration::from_secs(3_600),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        match build_edit_form(t) {
            Err(AppError::Unprocessable { .. }) => {}
            Ok(_) => panic!("expected Unprocessable"),
            Err(other) => panic!("expected Unprocessable, got {other:?}"),
        }
    }
}
