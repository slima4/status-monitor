//! Single source for turning a probe's terse error code into display text,
//! shared by the web views, the JSON API, and MCP.

use crate::domain::strip_served_stale;

pub fn humanize_check_error(raw: &str) -> String {
    let raw = match strip_served_stale(raw) {
        Some(r) => r,
        None => return "using last known result".into(),
    };
    if raw.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw)
            && let Some(days) = v.get("days_remaining").and_then(|d| d.as_i64())
        {
            if let Some(cn) = v.get("subject_common_name").and_then(|s| s.as_str()) {
                return fmt_days_label("cert", days, cn);
            }
            if let Some(domain) = v.get("domain").and_then(|s| s.as_str()) {
                return fmt_days_label("domain", days, domain);
            }
        }
        // Never hand a raw JSON blob to a reader.
        return "check failed".into();
    }
    match raw {
        "timeout" => "timed out".into(),
        "connect timeout" => "couldn't connect (timed out)".into(),
        "no response" => "connected, but no response (timed out)".into(),
        "body timeout" => "response body timed out".into(),
        "tls" => "TLS handshake failed".into(),
        "connect" => "connection failed".into(),
        "transport" => "transport error".into(),
        "dns: domain not found" => "domain not found (DNS)".into(),
        "dns: no address records" => "no DNS address records".into(),
        "dns: lookup timed out" => "DNS lookup timed out".into(),
        "dns: lookup failed" => "DNS lookup failed".into(),
        other => other.into(),
    }
}

fn fmt_days_label(kind: &str, days: i64, name: &str) -> String {
    if days < 0 {
        format!("{kind} expired {} days ago · {name}", -days)
    } else if days == 0 {
        format!("{kind} expires today · {name}")
    } else {
        format!("{kind} expires in {days} days · {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SERVED_STALE_PREFIX;

    #[test]
    fn expands_terse_transport_codes() {
        assert_eq!(
            humanize_check_error("no response"),
            "connected, but no response (timed out)"
        );
        assert_eq!(humanize_check_error("tls"), "TLS handshake failed");
        assert_eq!(
            humanize_check_error("connect timeout"),
            "couldn't connect (timed out)"
        );
    }

    #[test]
    fn de_machines_dns_reasons() {
        assert_eq!(
            humanize_check_error("dns: domain not found"),
            "domain not found (DNS)"
        );
        assert_eq!(
            humanize_check_error("dns: lookup timed out"),
            "DNS lookup timed out"
        );
    }

    #[test]
    fn formats_tls_cert_json() {
        let raw = r#"{"days_remaining":12,"subject_common_name":"example.com"}"#;
        assert_eq!(
            humanize_check_error(raw),
            "cert expires in 12 days · example.com"
        );
    }

    #[test]
    fn formats_domain_expiry_json() {
        let raw = r#"{"days_remaining":-3,"domain":"example.com"}"#;
        assert_eq!(
            humanize_check_error(raw),
            "domain expired 3 days ago · example.com"
        );
    }

    #[test]
    fn never_leaks_raw_json() {
        assert_eq!(humanize_check_error(r#"{"weird":"shape"}"#), "check failed");
    }

    #[test]
    fn passes_through_clean_reasons_and_unknown() {
        assert_eq!(
            humanize_check_error("connection refused"),
            "connection refused"
        );
        assert_eq!(
            humanize_check_error("certificate expired"),
            "certificate expired"
        );
    }

    #[test]
    fn served_stale_with_payload_decodes_and_hides_annotation() {
        let raw = r#"served_stale: last_verified_age_secs=3600; refresh_failed=whois_timeout; {"domain":"example.com","days_remaining":5}"#;
        let out = humanize_check_error(raw);
        assert_eq!(out, "domain expires in 5 days · example.com");
        assert!(!out.contains("served_stale"));
        assert!(!out.contains("refresh_failed"));
    }

    #[test]
    fn served_stale_without_renderable_is_generic() {
        assert_eq!(
            humanize_check_error(SERVED_STALE_PREFIX),
            "using last known result"
        );
        assert_eq!(
            humanize_check_error("served_stale: last_verified_age_secs=10"),
            "using last known result"
        );
    }
}
