//! Anti-abuse admission control: a compiled URL-pattern deny-list and a
//! domain deny-list (loaded from a YAML file), checked at target-creation
//! time so a denylisted target never enters the scheduler in the first place.
//!
//! Built once at startup from [`AbuseConfig`]. [`AbuseGuard::validate`] is the
//! strict gate `main` runs at config-validation time: a malformed regex or
//! YAML file is a clean startup error there, never a panic in construction
//! (the enforcement invariant on config-derived values). The runtime
//! [`AbuseGuard::from_config`] is total — it degrades (logs + skips) rather
//! than failing, because by the time it runs `validate` has already proven
//! the inputs good.

use std::collections::HashSet;
use std::sync::Arc;

use arc_swap::ArcSwap;
use regex::RegexSet;
use serde::Deserialize;
use url::Url;

use crate::api::error::codes;
use crate::config::AbuseConfig;
use crate::domain::CheckSpec;
use crate::error::{AppError, Result};

/// What an abuse match was. Drives the audit `quota_name`, the API error
/// code, and the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbuseKind {
    UrlPattern,
    Domain,
}

/// A positive abuse match: the rule that fired and the offending detail
/// (the matched pattern, or the deny-listed domain/parent).
#[derive(Debug, Clone)]
pub struct AbuseHit {
    pub kind: AbuseKind,
    pub detail: String,
}

impl AbuseHit {
    /// `quota_events.quota_name` value for this block.
    pub fn quota_name(&self) -> &'static str {
        match self.kind {
            AbuseKind::UrlPattern => "url_pattern",
            AbuseKind::Domain => "domain_denylist",
        }
    }

    /// 400 error carrying the stable code. URL-pattern matches surface as
    /// the generic `ABUSE_BLOCKED`; domain matches as `DOMAIN_DENYLISTED`.
    pub fn into_app_error(self) -> AppError {
        let (code, msg) = match self.kind {
            AbuseKind::UrlPattern => (
                codes::ABUSE_BLOCKED,
                "target URL matches a blocked abuse pattern".to_string(),
            ),
            AbuseKind::Domain => (
                codes::DOMAIN_DENYLISTED,
                format!("target domain '{}' is on the deny-list", self.detail),
            ),
        };
        AppError::bad_request_field(code, msg, "check.url")
    }
}

/// YAML shape of `config/abuse_denylist.yaml`. Only `domain` is consumed;
/// `category` / `reported_at` are operator-facing metadata and tolerated.
#[derive(Debug, Deserialize)]
struct DenylistFile {
    #[serde(default)]
    domains: Vec<DenylistEntry>,
}

#[derive(Debug, Deserialize)]
struct DenylistEntry {
    domain: String,
}

/// The hot-swappable rule set. [`AbuseGuard::reload`] replaces it
/// atomically; readers in [`AbuseGuard::inspect`] take a cheap `ArcSwap`
/// snapshot, so a reload never blocks a scheduler check and an in-flight
/// check always sees one consistent set (never a half-updated mix).
struct AbuseState {
    url_patterns: RegexSet,
    domain_denylist: HashSet<String>,
}

pub struct AbuseGuard {
    state: ArcSwap<AbuseState>,
}

impl AbuseGuard {
    /// Strict config-validation gate. Compiles every URL pattern and parses
    /// the deny-list file, returning a field-named error on the first
    /// problem. `main` calls this so a bad pattern / YAML is a clean startup
    /// config error rather than a silent degrade or a boot panic.
    pub fn validate(cfg: &AbuseConfig) -> Result<()> {
        for p in &cfg.url_patterns_denied {
            regex::Regex::new(p).map_err(|e| {
                AppError::Other(anyhow::anyhow!(
                    "abuse.url_patterns_denied: invalid regex {p:?}: {e}"
                ))
            })?;
        }
        if let Some(raw) = read_denylist_file(&cfg.domain_denylist_path)? {
            serde_norway::from_str::<DenylistFile>(&raw).map_err(|e| {
                AppError::Other(anyhow::anyhow!(
                    "abuse.domain_denylist_path ({}): invalid YAML: {e}",
                    cfg.domain_denylist_path
                ))
            })?;
        }
        Ok(())
    }

    /// Build the live guard. Total by design: an individually-bad pattern is
    /// logged and skipped, a missing file is an empty deny-list with a warn.
    /// In practice [`Self::validate`] ran first, so these paths are unreached.
    pub fn from_config(cfg: &AbuseConfig) -> Self {
        Self {
            state: ArcSwap::from_pointee(Self::build_state(cfg)),
        }
    }

    /// Re-read the URL patterns and deny-list file and swap them in
    /// atomically. The candidate config is validated first, so a malformed
    /// edit (bad regex / unparseable YAML) is rejected here and the running
    /// rules stay intact — a reload can only ever replace the live set with
    /// a fully-good one, never degrade it. Triggered by SIGHUP from `main`
    /// when `abuse.hot_reload_enabled`.
    pub fn reload(&self, cfg: &AbuseConfig) -> Result<()> {
        Self::validate(cfg)?;
        self.state.store(Arc::new(Self::build_state(cfg)));
        Ok(())
    }

    fn build_state(cfg: &AbuseConfig) -> AbuseState {
        let valid: Vec<&String> = cfg
            .url_patterns_denied
            .iter()
            .filter(|p| match regex::Regex::new(p) {
                Ok(_) => true,
                Err(e) => {
                    tracing::error!(pattern = %p, error = %e, "skipping invalid abuse URL pattern");
                    false
                }
            })
            .collect();
        // `valid` are already known-good, so the case-insensitive set build
        // cannot fail; fall back to an empty set if it somehow does.
        let url_patterns = RegexSet::new(valid.iter().map(|p| format!("(?i){p}")))
            .unwrap_or_else(|_| RegexSet::empty());

        let domain_denylist = match read_denylist_file(&cfg.domain_denylist_path) {
            Ok(Some(raw)) => match serde_norway::from_str::<DenylistFile>(&raw) {
                Ok(f) => f
                    .domains
                    .into_iter()
                    .map(|e| e.domain.trim().trim_end_matches('.').to_lowercase())
                    .filter(|d| !d.is_empty())
                    .collect(),
                Err(e) => {
                    tracing::error!(error = %e, "abuse deny-list YAML parse failed; empty deny-list");
                    HashSet::new()
                }
            },
            Ok(None) => {
                tracing::warn!(
                    path = %cfg.domain_denylist_path,
                    "abuse deny-list file absent; domain deny-list empty"
                );
                HashSet::new()
            }
            Err(e) => {
                tracing::error!(error = %e, "abuse deny-list read failed; empty deny-list");
                HashSet::new()
            }
        };

        AbuseState {
            url_patterns,
            domain_denylist,
        }
    }

    /// The reputation hook reserved for v1.x (domain-reputation API, abuse
    /// list, etc.). The call site is wired; the implementation is empty.
    fn reputation_check(&self, _url: &Url) -> Result<(), AbuseHit> {
        Ok(())
    }

    /// First abuse rule the check trips, if any. URL patterns apply to the
    /// full HTTP URL; the domain deny-list (with parent-domain matching)
    /// applies to every check kind's host/domain.
    pub fn inspect(&self, check: &CheckSpec) -> Option<AbuseHit> {
        // One snapshot per check: a concurrent reload swaps the whole set,
        // so this check sees either fully the old or fully the new rules.
        let s = self.state.load();
        match check {
            CheckSpec::Http(h) => {
                if let Some(i) = s.url_patterns.matches(h.url.as_str()).iter().next() {
                    return Some(AbuseHit {
                        kind: AbuseKind::UrlPattern,
                        detail: format!("pattern #{i}"),
                    });
                }
                // `host_str()` keeps the `[ ]` on an IPv6 literal; that's
                // fine — IP literals are never deny-list domain entries and
                // are governed by the SSRF guard, so `domain_hit` no-ops.
                if let Some(hit) = h.url.host_str().and_then(|host| s.domain_hit(host)) {
                    return Some(hit);
                }
                self.reputation_check(&h.url).err()
            }
            CheckSpec::Tcp(t) => s.domain_hit(crate::security::unbracket(&t.host)),
            CheckSpec::TlsCert(c) => s.domain_hit(crate::security::unbracket(&c.host)),
            CheckSpec::DomainExpiry(d) => s.domain_hit(&d.domain),
        }
    }
}

impl AbuseState {
    fn domain_hit(&self, host: &str) -> Option<AbuseHit> {
        if self.domain_denylist.is_empty() {
            return None;
        }
        domain_and_parents(host).into_iter().find_map(|d| {
            self.domain_denylist.contains(&d).then_some(AbuseHit {
                kind: AbuseKind::Domain,
                detail: d,
            })
        })
    }
}

/// `host` plus every parent domain down to (and including) the registrable
/// two-label tail — never a bare TLD. `a.b.example.com` →
/// `[a.b.example.com, b.example.com, example.com]`.
fn domain_and_parents(host: &str) -> Vec<String> {
    let host = host.trim().trim_end_matches('.').to_lowercase();
    if host.is_empty() {
        return Vec::new();
    }
    let labels: Vec<&str> = host.split('.').collect();
    (0..labels.len().saturating_sub(1))
        .map(|i| labels[i..].join("."))
        .collect()
}

/// Reads the deny-list file. `Ok(None)` = file absent (not an error — a
/// deployment may legitimately omit it); `Err` = present but unreadable.
fn read_denylist_file(path: &str) -> Result<Option<String>> {
    if path.trim().is_empty() {
        return Ok(None);
    }
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::Other(anyhow::anyhow!(
            "abuse.domain_denylist_path ({path}): {e}"
        ))),
    }
}

#[cfg(test)]
impl AbuseHit {
    fn into_app_error_code(self) -> &'static str {
        match self.into_app_error() {
            AppError::BadRequest { code, .. } => code,
            _ => "UNEXPECTED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(patterns: &[&str], domains: &[&str]) -> AbuseGuard {
        AbuseGuard {
            state: ArcSwap::from_pointee(AbuseState {
                url_patterns: RegexSet::new(patterns.iter().map(|p| format!("(?i){p}"))).unwrap(),
                domain_denylist: domains.iter().map(|d| d.to_string()).collect(),
            }),
        }
    }

    fn http(url: &str) -> CheckSpec {
        use crate::domain::{ExpectedStatus, HttpCheck, HttpMethod};
        CheckSpec::Http(HttpCheck {
            url: url.parse().unwrap(),
            method: HttpMethod::Get,
            timeout: std::time::Duration::from_secs(5),
            follow_redirects: false,
            max_redirects: 0,
            expected_status: ExpectedStatus::Exact(200),
            expected_body_contains: None,
            headers: Default::default(),
            body: None,
            verify_tls: true,
            basic_auth: None,
            bearer_token: None,
        })
    }

    #[test]
    fn url_pattern_blocks_git_dir() {
        let g = guard(&[r"/\.git(/|$)"], &[]);
        let hit = g.inspect(&http("https://example.com/.git/config")).unwrap();
        assert_eq!(hit.kind, AbuseKind::UrlPattern);
        assert_eq!(hit.into_app_error_code(), codes::ABUSE_BLOCKED);
    }

    #[test]
    fn url_pattern_is_case_insensitive_and_unanchored() {
        let g = guard(&[r"/wp-admin"], &[]);
        assert!(g.inspect(&http("https://x.com/path/WP-Admin/")).is_some());
    }

    #[test]
    fn clean_url_passes() {
        let g = guard(&[r"/\.git(/|$)"], &["status.betterstack.com"]);
        assert!(g.inspect(&http("https://example.com/healthz")).is_none());
    }

    #[test]
    fn domain_denylist_matches_exact_and_subdomain() {
        let g = guard(&[], &["status.betterstack.com"]);
        let hit = g.inspect(&http("https://status.betterstack.com/")).unwrap();
        assert_eq!(hit.kind, AbuseKind::Domain);
        // Parent-domain match: a sub-subdomain of a denied domain is denied.
        assert!(
            g.inspect(&http("https://eu.status.betterstack.com/"))
                .is_some()
        );
        // A sibling that merely shares the registrable tail is NOT denied
        // (only `status.betterstack.com` and below are listed).
        assert!(g.inspect(&http("https://betterstack.com/")).is_none());
    }

    #[test]
    fn domain_and_parents_stops_at_two_labels() {
        assert_eq!(
            domain_and_parents("a.b.example.com"),
            vec!["a.b.example.com", "b.example.com", "example.com"]
        );
        assert_eq!(domain_and_parents("example.com"), vec!["example.com"]);
        assert!(domain_and_parents("localhost").is_empty());
    }

    #[test]
    fn empty_denylist_never_matches_domain() {
        let g = guard(&[], &[]);
        assert!(
            g.inspect(&http("https://status.betterstack.com/"))
                .is_none()
        );
    }

    #[test]
    fn reload_swaps_in_the_new_denylist_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("denylist.yaml");
        std::fs::write(&path, "domains:\n  - { domain: \"old.example.com\" }\n").unwrap();
        let cfg = AbuseConfig {
            url_patterns_denied: vec![],
            domain_denylist_path: path.to_string_lossy().into_owned(),
            hot_reload_enabled: true,
        };
        let g = AbuseGuard::from_config(&cfg);
        assert!(g.inspect(&http("https://old.example.com/")).is_some());
        assert!(g.inspect(&http("https://new.example.com/")).is_none());

        // An operator edits the file, then SIGHUPs the process.
        std::fs::write(&path, "domains:\n  - { domain: \"new.example.com\" }\n").unwrap();
        g.reload(&cfg).expect("valid reload");

        // The swap is total: the new rule is live, the dropped one is gone.
        assert!(g.inspect(&http("https://new.example.com/")).is_some());
        assert!(g.inspect(&http("https://old.example.com/")).is_none());
        // `dir` drops here, removing the file even on an earlier panic.
    }

    #[test]
    fn reload_rejects_a_bad_edit_and_keeps_the_running_rules() {
        let g = guard(&[r"/\.git(/|$)"], &["blocked.example.com"]);
        assert!(g.inspect(&http("https://blocked.example.com/")).is_some());

        // A malformed regex must not take the live rules down with it.
        let bad = AbuseConfig {
            url_patterns_denied: vec!["(unclosed".to_string()],
            domain_denylist_path: String::new(),
            hot_reload_enabled: true,
        };
        assert!(g.reload(&bad).is_err(), "a bad config must be rejected");
        assert!(
            g.inspect(&http("https://blocked.example.com/")).is_some(),
            "the previous rules must stay intact after a rejected reload"
        );
    }
}
