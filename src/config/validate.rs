//! Startup validation: a bad number is a named config error, never a panic in
//! router or layer construction.

use secrecy::ExposeSecret;

use crate::error::Result;

use super::AppConfig;

impl AppConfig {
    /// Reject `< 1` quota / rate / interval values at load with a
    /// field-named error (I6). A bad number is a clean startup *config*
    /// error, never a `.expect()` crash-loop in router/layer construction.
    pub fn validate_quotas_and_limits(&self) -> Result<()> {
        fn ge1_u64(v: u64, field: &str) -> Result<()> {
            if v < 1 {
                return Err(crate::error::AppError::Other(anyhow::anyhow!(
                    "{field} must be >= 1 (got {v})"
                )));
            }
            Ok(())
        }
        ge1_u64(
            self.quotas.plan_cache_ttl_secs,
            "quotas.plan_cache_ttl_secs",
        )?;
        ge1_u64(
            self.quotas.usage_cache_ttl_secs,
            "quotas.usage_cache_ttl_secs",
        )?;
        ge1_u64(
            self.rate_limits.janitor.cleanup_interval_hours,
            "rate_limits.janitor.cleanup_interval_hours",
        )?;
        ge1_u64(
            self.rate_limits.janitor.idle_threshold_hours,
            "rate_limits.janitor.idle_threshold_hours",
        )?;
        ge1_u64(
            self.scheduler.target_refresh_interval_secs,
            "scheduler.target_refresh_interval_secs",
        )?;
        if self.checker.per_host_max_inflight == 0 {
            return Err(crate::error::AppError::Other(anyhow::anyhow!(
                "checker.per_host_max_inflight must be >= 1"
            )));
        }
        if self.checker.rdap_max_inflight == 0 {
            return Err(crate::error::AppError::Other(anyhow::anyhow!(
                "checker.rdap_max_inflight must be >= 1"
            )));
        }
        if self.escalation.reconcile_window_secs <= self.escalation.tick_interval_secs {
            return Err(crate::error::AppError::Other(anyhow::anyhow!(
                "escalation.reconcile_window_secs ({}) must exceed tick_interval_secs ({}) \
                 or the reconcile scan never matches",
                self.escalation.reconcile_window_secs,
                self.escalation.tick_interval_secs
            )));
        }
        if self.escalation.enabled {
            ge1_u64(
                self.escalation.tick_interval_secs,
                "escalation.tick_interval_secs",
            )?;
            if self.escalation.max_attempts < 1 {
                return Err(crate::error::AppError::Other(anyhow::anyhow!(
                    "escalation.max_attempts must be >= 1 (got {})",
                    self.escalation.max_attempts
                )));
            }
        }
        // Zero is "keep nothing", not "keep nothing older than the window".
        for (days, field) in [
            (
                self.retention.login_attempts_days,
                "retention.login_attempts_days",
            ),
            (
                self.retention.quota_events_days,
                "retention.quota_events_days",
            ),
            (self.retention.audit_log_days, "retention.audit_log_days"),
            (self.retention.mcp_audit_days, "retention.mcp_audit_days"),
            (
                self.auth.session.idle_timeout_days,
                "auth.session.idle_timeout_days",
            ),
            (
                self.tenancy.deletion_grace_period_days,
                "tenancy.deletion_grace_period_days",
            ),
        ] {
            ge1_u64(u64::from(days), field)?;
        }
        Ok(())
    }

    /// Marketing-site boot invariants. Cheap startup errors, never
    /// panics in router construction. Skipped wholesale when
    /// `marketing.enabled = false` so self-host deployments need not set
    /// any of these.
    pub fn validate_marketing(&self) -> Result<()> {
        fn err(msg: String) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg))
        }
        let m = &self.marketing;
        if !m.enabled {
            return Ok(());
        }
        let base = self.public_status.base_domain.trim();
        if base.is_empty() || !base.contains('.') {
            return Err(err(format!(
                "marketing.enabled = true requires public_status.base_domain to be a non-empty FQDN (got {base:?})"
            )));
        }
        for (field, value) in [
            ("marketing.canonical_origin", m.canonical_origin.as_str()),
            ("marketing.app_url", m.app_url.as_str()),
        ] {
            let v = value.trim();
            if v.is_empty() {
                return Err(err(format!(
                    "{field} is required when marketing.enabled = true"
                )));
            }
            if !v.starts_with("https://") {
                return Err(err(format!("{field} must start with https:// (got {v:?})")));
            }
            if v.ends_with('/') {
                return Err(err(format!(
                    "{field} must not end with a trailing slash (got {v:?})"
                )));
            }
        }
        for sub in &m.reserved_subdomains {
            let lower = sub.to_ascii_lowercase();
            if !crate::domain::reserved_slugs::is_reserved(&lower) {
                return Err(err(format!(
                    "marketing.reserved_subdomains entry {sub:?} is not in \
                     domain::reserved_slugs::RESERVED_SLUGS — keep the two lists aligned"
                )));
            }
        }
        // The session cookie must not be scoped to a parent zone that the
        // marketing host inherits; otherwise the app's session ID rides
        // along to the apex and the marketing CDN cache becomes Vary:
        // Cookie. Host-only (empty Domain) is always safe.
        let cd = self.auth.session.cookie_domain.trim();
        if !cd.is_empty() {
            let stripped = cd.trim_start_matches('.');
            if stripped == base || base.ends_with(&format!(".{stripped}")) {
                return Err(err(format!(
                    "auth.session.cookie_domain={cd:?} overlaps marketing host {base:?}; \
                     leave cookie_domain empty (host-only) so the apex marketing surface \
                     is not Vary: Cookie"
                )));
            }
        }
        Ok(())
    }

    /// Trace-export config is a clean startup error when inconsistent,
    /// never a runtime panic. Credentials are required only when export
    /// is actually active (`tracing_enabled` AND `grafana.enabled`); the
    /// sample ratio is always range-checked.
    pub fn validate_observability(&self) -> Result<()> {
        fn err(msg: String) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg))
        }
        let g = &self.observability.grafana;
        let r = g.trace_sample_ratio;
        if !(0.0..=1.0).contains(&r) {
            return Err(err(format!(
                "observability.grafana.trace_sample_ratio must be in [0.0, 1.0] (got {r})"
            )));
        }
        if self.observability.tracing_enabled && g.enabled {
            if g.otlp_endpoint.trim().is_empty() {
                return Err(err(
                    "observability.grafana.otlp_endpoint is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
            if g.instance_id.trim().is_empty() {
                return Err(err(
                    "observability.grafana.instance_id is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
            if g.api_key.expose_secret().trim().is_empty() {
                return Err(err(
                    "UPTIMEPAGE_OBSERVABILITY__GRAFANA__API_KEY is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
        }
        let hb = &self.observability.heartbeat;
        if hb.enabled {
            if hb.url.trim().is_empty() {
                return Err(err(
                    "UPTIMEPAGE_OBSERVABILITY__HEARTBEAT__URL is required when observability.heartbeat.enabled is true".into(),
                ));
            }
            if hb.interval_seconds == 0 {
                return Err(err(
                    "observability.heartbeat.interval_seconds must be > 0".into()
                ));
            }
        }
        Ok(())
    }

    /// Central-bot invariants, enforced only when `telegram.bot_token` is set.
    /// A misconfigured bot here is a clean startup error rather than a half-up
    /// feature that mints dead deep links.
    pub fn validate_telegram(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let t = &self.telegram;
        if !t.enabled() {
            return Ok(());
        }
        if t.bot_username.trim().is_empty() {
            return Err(err(
                "telegram.bot_username is required when telegram.bot_token is set",
            ));
        }
        if t.webhook_secret.expose_secret().trim().len() < 32 {
            return Err(err(
                "UPTIMEPAGE_TELEGRAM__WEBHOOK_SECRET must be at least 32 chars when telegram.bot_token is set",
            ));
        }
        let base = self.auth.public_base_url.trim();
        match url::Url::parse(base) {
            Ok(u) if u.scheme() == "https" && u.host_str().is_some() => {}
            _ => {
                return Err(err(
                    "auth.public_base_url must be an https:// URL with a host for the telegram webhook",
                ));
            }
        }
        Ok(())
    }

    /// A half-configured Resend sender is a clean startup error, not a
    /// per-send failure after cutover. The webhook secret alone is fine —
    /// the bounce receiver works regardless of the sending provider.
    pub fn validate_email(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let e = &self.email;
        if e.provider != "resend" {
            return Ok(());
        }
        if e.resend.api_key.expose_secret().trim().is_empty() {
            return Err(err(
                "email.resend.api_key is required when email.provider = \"resend\"",
            ));
        }
        if e.from_address.trim().is_empty() {
            return Err(err(
                "email.from_address is required when email.provider = \"resend\"",
            ));
        }
        Ok(())
    }

    /// A half-configured operator WhatsApp number is a clean startup error,
    /// not a dead webhook or a failing send after the flag flip.
    pub fn validate_whatsapp_app(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let w = &self.whatsapp_app;
        if !w.enabled {
            return Ok(());
        }
        if w.access_token.expose_secret().trim().is_empty()
            || w.phone_number_id.trim().is_empty()
            || w.app_secret.expose_secret().trim().is_empty()
            || w.verify_token.expose_secret().trim().is_empty()
        {
            return Err(err(
                "whatsapp_app.enabled needs access_token, phone_number_id, app_secret and verify_token set",
            ));
        }
        if w.verify_token.expose_secret().trim().len() < 32 {
            return Err(err(
                "UPTIMEPAGE_WHATSAPP_APP__VERIFY_TOKEN must be at least 32 chars",
            ));
        }
        let n = w.public_number.trim();
        if !(5..=20).contains(&n.len()) || !n.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err(
                "whatsapp_app.public_number must be the display number as international digits",
            ));
        }
        if w.template_name.is_empty()
            || !w
                .template_name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(err(
                "whatsapp_app.template_name is required (lowercase letters, digits, and _ only)",
            ));
        }
        if w.language_code.is_empty()
            || !w
                .language_code
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(err(
                "whatsapp_app.language_code must be a code like en or en_US",
            ));
        }
        // Deliberate: sends are operator-paid Meta template messages with no
        // per-org cap yet — the flag flip is the only spend control.
        tracing::warn!(
            "whatsapp_app.enabled — operator-paid template sends are UNCAPPED; \
             monitor spend until per-org send caps land"
        );
        Ok(())
    }

    /// Validate the regional-agent section. Only enforced when `agent.enabled`.
    pub fn validate_runtime(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let agent = &self.agent;
        if !agent.enabled {
            return Ok(());
        }
        if agent.control_plane_url.trim().is_empty() {
            return Err(err(
                "agent.control_plane_url is required when agent.enabled",
            ));
        }
        // Resolved secrets and decrypted credentials ride the config-pull
        // response, so the control-plane transport must be encrypted. Cleartext
        // is permitted only when private targets are explicitly opted in (a
        // trusted private-network or localhost control plane for dev/integration).
        let url = url::Url::parse(agent.control_plane_url.trim())
            .map_err(|_| err("agent.control_plane_url is not a valid URL"))?;
        if url.scheme() != "https" && !self.security.allow_private_targets {
            return Err(err(
                "agent.control_plane_url must use https; cleartext is permitted \
                 only with security.allow_private_targets for a trusted \
                 private-network or localhost control plane",
            ));
        }
        if agent.region.trim().is_empty() {
            return Err(err("agent.region is required when agent.enabled"));
        }
        if agent.token.expose_secret().trim().is_empty() {
            return Err(err(
                "UPTIMEPAGE_AGENT__TOKEN is required when agent.enabled is true",
            ));
        }
        if agent.pull_interval_secs == 0 {
            return Err(err("agent.pull_interval_secs must be > 0"));
        }
        if agent.buffer_capacity == 0 {
            return Err(err("agent.buffer_capacity must be > 0"));
        }
        Ok(())
    }

    /// Reject the published `monitor` credentials; unoverridden they expose every tenant row.
    pub fn validate_storage(&self) -> Result<()> {
        const SHIPPED: &str = "monitor";
        const OPT_IN: &str = "set UPTIMEPAGE_STORAGE__ALLOW_DEFAULT_CREDENTIALS=true \
                              for a local stack";
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        if self.storage.allow_default_credentials {
            return Ok(());
        }
        let pg = url::Url::parse(&self.storage.postgres.url)
            .map_err(|_| err("storage.postgres.url is not a valid URL"))?;
        if pg.username() == SHIPPED && pg.password() == Some(SHIPPED) {
            return Err(err(&format!(
                "storage.postgres.url still carries the shipped credentials; \
                 set UPTIMEPAGE_STORAGE__POSTGRES__URL, or {OPT_IN}"
            )));
        }
        let ch = self.storage.clickhouse.password.expose_secret();
        if ch.is_empty() || ch == SHIPPED {
            return Err(err(&format!(
                "storage.clickhouse.password is empty or still the shipped value; \
                 set UPTIMEPAGE_STORAGE__CLICKHOUSE__PASSWORD, or {OPT_IN}"
            )));
        }
        Ok(())
    }
}
