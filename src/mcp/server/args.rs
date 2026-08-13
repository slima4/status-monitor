use super::text::sanitize_data;
use super::view::{channel_names, region_policy_str, sorted, tag_list};
use super::{DEFAULT_INCIDENT_WINDOW_DAYS, MAX_INCIDENT_WINDOW_DAYS, bound_ids, resolve_bindings};
use crate::mcp::error::config_error;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::domain::notification_channel::NotificationChannel;
use crate::domain::public::IncidentStatusPhase;
use crate::domain::target::{RegionIncidentPolicy, Target, TargetUpdate};
use crate::domain::{CheckSpec, ExpectedStatus};
use crate::storage::TimeRange;

use crate::mcp::error::McpToolError;
use crate::mcp::schema::{
    FieldChange, NewCheck, RegionPolicyArg, RegionPolicyMode, UpdateMonitorArgs,
};

/// The patch to apply plus one [`FieldChange`] per field that moves. A field
/// sent with the value it already has is dropped from both.
pub(super) fn build_monitor_patch(
    args: &UpdateMonitorArgs,
    target: &Target,
    channels: &[NotificationChannel],
) -> Result<(TargetUpdate, Vec<FieldChange>), McpToolError> {
    let mut update = TargetUpdate::default();
    let mut changes = Vec::new();
    let mut moved = |field: &str, from: String, to: String| {
        changes.push(FieldChange {
            field: field.to_string(),
            from,
            to,
        });
    };

    if let Some(secs) = args.interval_secs
        && secs != target.interval.as_secs()
    {
        fits_i32(secs, "interval_secs")?;
        moved(
            "interval_secs",
            target.interval.as_secs().to_string(),
            secs.to_string(),
        );
        update.interval = Some(std::time::Duration::from_secs(secs));
    }
    if let Some(n) = args.alert_confirmations
        && n != target.alert_confirmations
    {
        fits_i32(u64::from(n), "alert_confirmations")?;
        moved(
            "alert_confirmations",
            target.alert_confirmations.to_string(),
            n.to_string(),
        );
        update.alert_confirmations = Some(n);
    }
    if let Some(on) = args.notify_recovery
        && on != target.notify_recovery
    {
        moved(
            "notify_recovery",
            target.notify_recovery.to_string(),
            on.to_string(),
        );
        update.notify_recovery = Some(on);
    }
    if let Some(secs) = args.renotify_interval_secs
        && secs != target.renotify_interval_secs
    {
        fits_i32(u64::from(secs), "renotify_interval_secs")?;
        moved(
            "renotify_interval_secs",
            target.renotify_interval_secs.to_string(),
            secs.to_string(),
        );
        update.renotify_interval_secs = Some(secs);
    }
    if let Some(tags) = args.tags.as_ref() {
        let tags = crate::api::handlers::targets::normalize_tags(tags).map_err(config_error)?;
        if sorted(&tags) != sorted(&target.tags) {
            moved("tags", tag_list(&target.tags), tag_list(&tags));
            update.tags = Some(tags);
        }
    }
    if let Some(group) = args.group_name.as_ref() {
        let group = match group.as_deref().map(str::trim) {
            Some("") => {
                return Err(McpToolError::invalid_argument(
                    "group_name must not be blank; send null to clear it",
                ));
            }
            other => other.map(str::to_string),
        };
        if group != target.group_name {
            let shown = |g: &Option<String>| {
                g.as_deref()
                    .map(sanitize_data)
                    .unwrap_or("none".to_string())
            };
            moved("group_name", shown(&target.group_name), shown(&group));
            update.group_name = Some(group);
        }
    }
    if let Some(policy) = args.region_policy.as_ref() {
        let policy = parse_region_policy(policy)?;
        if policy != target.region_policy {
            moved(
                "region_policy",
                region_policy_str(target.region_policy),
                region_policy_str(policy),
            );
            update.region_policy = Some(policy);
        }
    }
    if let Some(ids) = args.channel_ids.as_ref() {
        let alerts = resolve_bindings(ids, channels)?;
        // A set, not a sequence: the same channels in another order alert the
        // same people, and calling that a change spends a confirmation on one.
        if bound_ids(&alerts) != bound_ids(&target.alerts) {
            moved(
                "alerts",
                channel_names(&target.alerts, channels),
                channel_names(&alerts, channels),
            );
            update.alerts = Some(alerts);
        }
    }
    Ok((update, changes))
}

/// The cadence a caller gets for omitting one: where the app's own picker opens
/// a monitor of this kind, raised to the plan floor. The hard minimum would be
/// legal but far noisier, probing a certificate twelve times more often than
/// any other front door does. A heartbeat is capped at its own window, since a
/// tick coarser than that could never judge it.
pub(super) fn default_interval_secs(check: &CheckSpec, plan_floor_secs: u64) -> u64 {
    let opening = plan_floor_secs.max(crate::domain::interval_hints_for_kind(check.kind()).default);
    match check.as_heartbeat() {
        // Never below the floor: a default the plan forbids would be refused
        // as an argument the caller never sent.
        Some(hb) => opening
            .min(hb.period.as_secs().saturating_add(hb.grace.as_secs()))
            .max(plan_floor_secs),
        None => opening,
    }
}

/// Field names for the audit row on a call that failed before the diff existed.
pub(super) fn requested_fields(args: &UpdateMonitorArgs) -> Vec<&'static str> {
    [
        ("interval_secs", args.interval_secs.is_some()),
        ("alert_confirmations", args.alert_confirmations.is_some()),
        ("notify_recovery", args.notify_recovery.is_some()),
        (
            "renotify_interval_secs",
            args.renotify_interval_secs.is_some(),
        ),
        ("tags", args.tags.is_some()),
        ("group_name", args.group_name.is_some()),
        ("region_policy", args.region_policy.is_some()),
        ("channel_ids", args.channel_ids.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, sent)| sent.then_some(name))
    .collect()
}

/// The column is `integer`; an out-of-range count would wrap rather than fail.
pub(super) fn fits_i32(secs: u64, field: &str) -> Result<(), McpToolError> {
    if secs > i32::MAX as u64 {
        return Err(McpToolError::invalid_argument(format!(
            "{field} must be at most {} seconds",
            i32::MAX
        )));
    }
    Ok(())
}

pub(super) fn parse_region_policy(
    arg: &RegionPolicyArg,
) -> Result<RegionIncidentPolicy, McpToolError> {
    match arg.mode {
        RegionPolicyMode::Any => Ok(RegionIncidentPolicy::Any),
        RegionPolicyMode::Majority => Ok(RegionIncidentPolicy::Majority),
        RegionPolicyMode::All => Ok(RegionIncidentPolicy::All),
        RegionPolicyMode::Count => arg.count.map(RegionIncidentPolicy::Count).ok_or_else(|| {
            McpToolError::invalid_argument("region_policy mode `count` needs a `count`")
        }),
    }
}

/// The narrow create surface widened into a real check, with the fields this
/// tool refuses to take left at their defaults.
pub(super) fn new_check_spec(check: &NewCheck) -> Result<CheckSpec, McpToolError> {
    use crate::domain::{
        DnsCheck, DomainExpiryCheck, HeartbeatCheck, HttpCheck, PingCheck, TcpCheck, TlsCertCheck,
    };
    use std::time::Duration;

    let ms = |v: Option<u64>, default: u64| Duration::from_millis(v.unwrap_or(default));
    Ok(match check {
        NewCheck::Http {
            url,
            method,
            expected_status,
            expected_body_contains,
            timeout_ms,
            follow_redirects,
            verify_tls,
        } => {
            let url = url::Url::parse(url)
                .map_err(|e| McpToolError::invalid_argument(format!("url: {e}")))?;
            // Userinfo is a password by another name, and this tool refuses to
            // carry one. It would also be echoed back in the prompt and the
            // audit row, since `address` reports the URL as configured.
            if !url.username().is_empty() || url.password().is_some() {
                return Err(McpToolError::invalid_argument(
                    "url must not carry a username or password; add credentials to the monitor in the app",
                ));
            }
            let follow = follow_redirects.unwrap_or(true);
            CheckSpec::Http(HttpCheck {
                url,
                method: parse_http_method(method.as_deref())?,
                timeout: ms(*timeout_ms, 10_000),
                follow_redirects: follow,
                max_redirects: if follow { 5 } else { 0 },
                expected_status: parse_expected_status(expected_status.as_deref())?,
                expected_body_contains: expected_body_contains.clone(),
                headers: Default::default(),
                body: None,
                verify_tls: verify_tls.unwrap_or(true),
                basic_auth: None,
                bearer_token: None,
            })
        }
        NewCheck::Tcp {
            host,
            port,
            timeout_ms,
        } => CheckSpec::Tcp(TcpCheck {
            host: host.clone(),
            port: *port,
            timeout: ms(*timeout_ms, 5_000),
        }),
        NewCheck::Ping { host, timeout_ms } => CheckSpec::Ping(PingCheck {
            host: host.clone(),
            timeout: ms(*timeout_ms, 5_000),
        }),
        NewCheck::Dns {
            domain,
            record_type,
            resolver,
            expected_contains,
            timeout_ms,
        } => CheckSpec::Dns(DnsCheck {
            domain: domain.clone(),
            record_type: parse_record_type(record_type.as_deref())?,
            resolver: resolver.clone(),
            expected_contains: expected_contains.clone(),
            timeout: ms(*timeout_ms, 5_000),
        }),
        NewCheck::TlsCert {
            host,
            port,
            warn_days,
            critical_days,
            timeout_ms,
        } => CheckSpec::TlsCert(TlsCertCheck {
            host: host.clone(),
            port: port.unwrap_or(443),
            server_name: None,
            warn_days: warn_days.unwrap_or(30),
            critical_days: critical_days.unwrap_or(7),
            timeout: ms(*timeout_ms, 10_000),
        }),
        NewCheck::DomainExpiry {
            domain,
            warn_days,
            critical_days,
            timeout_ms,
        } => CheckSpec::DomainExpiry(DomainExpiryCheck {
            domain: domain.clone(),
            warn_days: warn_days.unwrap_or(30),
            critical_days: critical_days.unwrap_or(7),
            timeout: ms(*timeout_ms, 10_000),
        }),
        NewCheck::Heartbeat {
            period_secs,
            grace_secs,
            max_runtime_secs,
        } => CheckSpec::Heartbeat(HeartbeatCheck {
            period: Duration::from_secs(*period_secs),
            grace: Duration::from_secs(*grace_secs),
            max_runtime: max_runtime_secs.map(Duration::from_secs),
        }),
    })
}

pub(super) fn parse_http_method(
    method: Option<&str>,
) -> Result<crate::domain::HttpMethod, McpToolError> {
    use crate::domain::HttpMethod;
    Ok(
        match method.unwrap_or("get").to_ascii_lowercase().as_str() {
            "get" => HttpMethod::Get,
            "head" => HttpMethod::Head,
            "post" => HttpMethod::Post,
            "put" => HttpMethod::Put,
            "patch" => HttpMethod::Patch,
            "delete" => HttpMethod::Delete,
            "options" => HttpMethod::Options,
            other => {
                return Err(McpToolError::invalid_argument(format!(
                    "unknown method `{other}`; expected one of get, head, post, put, patch, delete, options"
                )));
            }
        },
    )
}

pub(super) fn parse_record_type(
    kind: Option<&str>,
) -> Result<crate::domain::DnsRecordType, McpToolError> {
    use crate::domain::DnsRecordType;
    Ok(match kind.unwrap_or("a").to_ascii_lowercase().as_str() {
        "a" => DnsRecordType::A,
        "aaaa" => DnsRecordType::Aaaa,
        "cname" => DnsRecordType::Cname,
        "mx" => DnsRecordType::Mx,
        "ns" => DnsRecordType::Ns,
        "txt" => DnsRecordType::Txt,
        "soa" => DnsRecordType::Soa,
        "ptr" => DnsRecordType::Ptr,
        "caa" => DnsRecordType::Caa,
        "srv" => DnsRecordType::Srv,
        other => {
            return Err(McpToolError::invalid_argument(format!(
                "unknown record_type `{other}`; expected one of a, aaaa, cname, mx, ns, txt, soa, ptr, caa, srv"
            )));
        }
    })
}

/// `200`, `200-299`, or `200,201,204`. The inverse of `expected_status_str`.
pub(super) fn parse_expected_status(spec: Option<&str>) -> Result<ExpectedStatus, McpToolError> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(ExpectedStatus::Range { min: 200, max: 299 });
    };
    let code = |s: &str| -> Result<u16, McpToolError> {
        s.trim().parse::<u16>().map_err(|_| {
            McpToolError::invalid_argument(format!(
                "expected_status `{spec}` is not a code, a range like 200-299, or a list like 200,201"
            ))
        })
    };
    if let Some((lo, hi)) = spec.split_once('-') {
        let (min, max) = (code(lo)?, code(hi)?);
        if min > max {
            return Err(McpToolError::invalid_argument(format!(
                "expected_status range `{spec}` starts above where it ends"
            )));
        }
        return Ok(ExpectedStatus::Range { min, max });
    }
    if spec.contains(',') {
        let codes = spec
            .split(',')
            .map(code)
            .collect::<Result<Vec<_>, McpToolError>>()?;
        return Ok(ExpectedStatus::OneOf(codes));
    }
    Ok(ExpectedStatus::Exact(code(spec)?))
}

/// Resolve a requested region against what the monitor is actually assigned to.
/// Naming the valid ids beats an empty answer, which reads as "healthy there".
pub(super) fn requested_region(
    requested: Option<&str>,
    assigned: &[String],
) -> Result<Option<String>, McpToolError> {
    let Some(region) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if !assigned.iter().any(|a| a == region) {
        return Err(McpToolError::invalid_argument(if assigned.is_empty() {
            "this monitor runs in no probe region, so it cannot be filtered by one".to_string()
        } else {
            format!(
                "monitor does not run in region `{}`; it runs in {}",
                sanitize_data(region),
                assigned.join(", ")
            )
        }));
    }
    Ok(Some(region.to_string()))
}

/// `open` (default) keeps only running incidents; `all` includes resolved ones.
pub(super) fn parse_incident_state_filter(state: Option<&str>) -> Result<bool, McpToolError> {
    match state {
        None | Some("open") => Ok(true),
        Some("all") => Ok(false),
        Some(other) => Err(McpToolError::invalid_argument(format!(
            "unknown state `{other}` (expected `open` or `all`)"
        ))),
    }
}

/// Resolve the caller's `from`/`to` into a bounded window: defaults to the
/// trailing [`DEFAULT_INCIDENT_WINDOW_DAYS`], and a span wider than
/// [`MAX_INCIDENT_WINDOW_DAYS`] is clamped by moving `from` forward.
pub(super) fn incident_window(
    from: Option<&str>,
    to: Option<&str>,
    now: DateTime<Utc>,
) -> Result<TimeRange, McpToolError> {
    let to = match to {
        Some(s) => parse_rfc3339(s, "to")?,
        None => now,
    };
    let from = match from {
        Some(s) => parse_rfc3339(s, "from")?,
        None => to - Duration::try_days(DEFAULT_INCIDENT_WINDOW_DAYS).unwrap_or_default(),
    };
    if from >= to {
        return Err(McpToolError::invalid_argument("`from` must be before `to`"));
    }
    let widest = Duration::try_days(MAX_INCIDENT_WINDOW_DAYS).unwrap_or_default();
    let from = from.max(to - widest);
    Ok(TimeRange { from, to })
}

pub(super) fn parse_rfc3339(value: &str, field: &str) -> Result<DateTime<Utc>, McpToolError> {
    DateTime::parse_from_rfc3339(value)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| {
            McpToolError::invalid_argument(format!("`{field}` must be an RFC 3339 timestamp"))
        })
}

pub(super) fn parse_uuid(s: &str, what: &str) -> Result<Uuid, McpToolError> {
    Uuid::parse_str(s).map_err(|_| McpToolError::invalid_argument(format!("invalid {what}")))
}

/// Window string → (span, latency bucket seconds). Bucket sizes target ~50-60
/// points across the window.
pub(super) fn parse_window(s: &str) -> Result<(Duration, u32), McpToolError> {
    let (hours, bucket) = match s {
        "1h" => (1, 60),
        "24h" => (24, 1_800),
        "7d" => (24 * 7, 10_800),
        "30d" => (24 * 30, 43_200),
        other => {
            return Err(McpToolError::invalid_argument(format!(
                "unknown window `{other}`; expected one of 1h, 24h, 7d, 30d"
            )));
        }
    };
    Ok((Duration::try_hours(hours).unwrap_or_default(), bucket))
}

/// Accepted monitor states for the `list_monitors` filter.
pub(super) fn parse_state(s: &str) -> Result<&'static str, McpToolError> {
    match s {
        "up" => Ok("up"),
        "down" => Ok("down"),
        "degraded" => Ok("degraded"),
        "error" => Ok("error"),
        "no_data" => Ok("no_data"),
        other => Err(McpToolError::invalid_argument(format!(
            "unknown state `{other}`; expected one of up, down, degraded, error, no_data"
        ))),
    }
}

/// Accepted incident phases for `post_incident_update`.
pub(super) fn parse_phase(s: &str) -> Result<IncidentStatusPhase, McpToolError> {
    match s {
        "investigating" => Ok(IncidentStatusPhase::Investigating),
        "identified" => Ok(IncidentStatusPhase::Identified),
        "monitoring" => Ok(IncidentStatusPhase::Monitoring),
        "resolved" => Ok(IncidentStatusPhase::Resolved),
        "postmortem" => Ok(IncidentStatusPhase::Postmortem),
        other => Err(McpToolError::invalid_argument(format!(
            "unknown phase `{other}`; expected one of investigating, identified, monitoring, resolved, postmortem"
        ))),
    }
}

/// Accepted monitor kinds for the `list_monitors` filter — derived from
/// `ALL_KINDS` so a new check kind is filterable without touching this file.
pub(super) fn parse_kind(s: &str) -> Result<&'static str, McpToolError> {
    crate::domain::CheckSpec::ALL_KINDS
        .into_iter()
        .find(|k| *k == s)
        .ok_or_else(|| {
            McpToolError::invalid_argument(format!(
                "unknown type `{s}`; expected one of {}",
                crate::domain::CheckSpec::ALL_KINDS.join(", ")
            ))
        })
}
