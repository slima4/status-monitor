//! Operator Monitors page. Three round-trips per request: PG `list`,
//! one batched CH `dashboard_rollup`, PG `list_members` for owners.

use std::collections::HashMap;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Query, State};
use chrono::{Duration as ChronoDuration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::types::DashboardMetrics;
use crate::app::AppState;
use crate::domain::{OrgId, Target};
use crate::storage::TimeRange;
use crate::storage::orgs::list_members;
use crate::storage::traits::{TargetFilter, TargetSort};
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::views::{describe_check, humanize_duration};
use crate::web::{AuthedBrowser, CurrentOrg};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const UPTIME_WINDOW_DAYS: i64 = 30;
const UNGROUPED_LABEL: &str = "Ungrouped";
const TYPE_CHIPS: &[&str] = &["HTTP", "TCP", "DNS", "TLS", "DOMAIN"];

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub owner: Option<Uuid>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

pub struct MonitorRow {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub interval_label: String,
    pub interval_s: u64,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub group_name: Option<String>,
    pub last_status: &'static str,
    pub status_class: &'static str,
    pub last_check_label: String,
    pub next_check_label: String,
    pub uptime_30d_label: String,
    pub owner: Option<OwnerView>,
}

pub struct OwnerView {
    pub id: Uuid,
    pub initials: String,
    pub label: String,
    pub color: String,
}

pub struct GroupBlock {
    pub name: String,
    /// `false` when bucketed under the `Ungrouped` sentinel — drives
    /// a header variant that hides the name.
    pub has_name: bool,
    pub rows: Vec<MonitorRow>,
    pub total: usize,
    pub worst_status: &'static str,
    pub avg_uptime_label: String,
}

pub struct TypeChip {
    pub label: &'static str,
    pub count: usize,
    pub active: bool,
}

pub struct OwnerOption {
    pub id: Uuid,
    pub label: String,
    pub initials: String,
    pub color: String,
    pub selected: bool,
}

pub struct GroupOption {
    pub name: String,
    pub selected: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/list.html")]
pub struct ListPage {
    pub active_tab: &'static str,
    pub groups: Vec<GroupBlock>,
    pub total: usize,
    pub paused_total: usize,
    pub type_chips: Vec<TypeChip>,
    pub owner_options: Vec<OwnerOption>,
    pub group_options: Vec<GroupOption>,
    pub has_more: bool,
    pub limit: usize,
    pub offset: usize,
    pub prev_offset: Option<usize>,
    pub next_offset: Option<usize>,
    pub q: String,
    pub tag: String,
    pub enabled: Option<bool>,
    pub group: String,
    pub owner: Option<Uuid>,
    pub kind: String,
    pub sort: &'static str,
    pub onboarding: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/partials/list_body.html")]
pub struct ListBodyPartial {
    pub groups: Vec<GroupBlock>,
    pub total: usize,
    pub paused_total: usize,
    pub type_chips: Vec<TypeChip>,
    /// Carried by the partial so toolbar selects refresh on every
    /// `#target-rows` swap; without these the chip-active class would
    /// drift away from the URL state.
    pub owner_options: Vec<OwnerOption>,
    pub group_options: Vec<GroupOption>,
    pub has_more: bool,
    pub limit: usize,
    pub offset: usize,
    pub prev_offset: Option<usize>,
    pub next_offset: Option<usize>,
    pub q: String,
    pub tag: String,
    pub enabled: Option<bool>,
    pub group: String,
    pub owner: Option<Uuid>,
    pub kind: String,
    pub sort: &'static str,
}

pub async fn index(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> WebResult<ListPage> {
    let page = build_page(&state, org, &params).await?;
    Ok(page)
}

pub async fn list_partial(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> WebResult<ListBodyPartial> {
    let page = build_page(&state, org, &params).await?;
    Ok(ListBodyPartial {
        groups: page.groups,
        total: page.total,
        paused_total: page.paused_total,
        type_chips: page.type_chips,
        owner_options: page.owner_options,
        group_options: page.group_options,
        has_more: page.has_more,
        limit: page.limit,
        offset: page.offset,
        prev_offset: page.prev_offset,
        next_offset: page.next_offset,
        q: page.q,
        tag: page.tag,
        enabled: page.enabled,
        group: page.group,
        owner: page.owner,
        kind: page.kind,
        sort: page.sort,
    })
}

async fn build_page(state: &AppState, org: OrgId, params: &ListParams) -> WebResult<ListPage> {
    let sort = parse_sort(params.sort.as_deref());
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);

    let trim_str = |s: &Option<String>| s.as_deref().map(str::trim).map(str::to_owned);
    let q = trim_str(&params.q)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let tag = trim_str(&params.tag)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let group = trim_str(&params.group)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let kind = trim_str(&params.kind)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    let filter = TargetFilter {
        limit: Some(limit + 1),
        offset,
        q: (!q.is_empty()).then(|| q.clone()),
        tag: (!tag.is_empty()).then(|| tag.clone()),
        enabled: params.enabled,
        group: (!group.is_empty()).then(|| group.clone()),
        owner: params.owner,
        sort,
    };

    let mut targets = state.target_store.list(org, filter).await?;
    let has_more = targets.len() > limit;
    if has_more {
        targets.truncate(limit);
    }

    // Tally BEFORE the kind retain so chip counts stay invariant when
    // the user clicks between chips.
    let mut chip_counts: HashMap<&'static str, usize> = HashMap::new();
    for t in &targets {
        let (k, _) = describe_check(&t.check);
        *chip_counts.entry(k).or_default() += 1;
    }
    let all_count = targets.len();

    if !kind.is_empty() {
        targets.retain(|t| describe_check(&t.check).0.eq_ignore_ascii_case(&kind));
    }

    let metrics_by_target: HashMap<Uuid, DashboardMetrics> = if targets.is_empty() {
        HashMap::new()
    } else {
        let now = Utc::now();
        let range = TimeRange {
            from: now - ChronoDuration::days(UPTIME_WINDOW_DAYS),
            to: now,
        };
        state
            .results_store
            .dashboard_rollup(org, range)
            .await?
            .into_iter()
            .map(|m| (m.target_id, m))
            .collect()
    };

    let members = match state.db.as_ref() {
        Some(pool) => list_members(pool, org).await?,
        None => Vec::new(),
    };
    let owner_lookup: HashMap<Uuid, MemberLite> = members
        .iter()
        .map(|m| {
            (
                m.membership.user_id.0,
                MemberLite {
                    id: m.membership.user_id.0,
                    label: m.email.clone(),
                },
            )
        })
        .collect();

    let now = Utc::now();
    let mut rows: Vec<MonitorRow> = Vec::with_capacity(targets.len());
    let mut paused_total = 0usize;
    for t in targets {
        let metrics = metrics_by_target.get(&t.id);
        let row = build_row(&t, metrics, &owner_lookup, now);
        if !row.enabled {
            paused_total += 1;
        }
        rows.push(row);
    }
    let total = rows.len();
    let groups = bucket_by_group(rows);

    let mut type_chips: Vec<TypeChip> = Vec::with_capacity(TYPE_CHIPS.len() + 1);
    type_chips.push(TypeChip {
        label: "All",
        count: all_count,
        active: kind.is_empty(),
    });
    for label in TYPE_CHIPS {
        type_chips.push(TypeChip {
            label,
            count: chip_counts.get(label).copied().unwrap_or(0),
            active: kind.eq_ignore_ascii_case(label),
        });
    }

    let mut owner_options: Vec<OwnerOption> = members
        .iter()
        .map(|m| {
            let id = m.membership.user_id.0;
            let label = m.email.clone();
            OwnerOption {
                id,
                initials: initials_from(&label),
                color: avatar_color(id),
                label,
                selected: Some(id) == params.owner,
            }
        })
        .collect();
    owner_options.sort_by(|a, b| a.label.cmp(&b.label));

    let group_options = collect_group_options(&groups, &group);

    let prev_offset = (offset > 0).then(|| offset.saturating_sub(limit));
    let next_offset = has_more.then_some(offset + limit);

    let onboarding = groups.is_empty()
        && q.is_empty()
        && tag.is_empty()
        && group.is_empty()
        && kind.is_empty()
        && params.owner.is_none()
        && params.enabled.is_none()
        && offset == 0;

    Ok(ListPage {
        active_tab: "targets",
        groups,
        total,
        paused_total,
        type_chips,
        owner_options,
        group_options,
        has_more,
        limit,
        offset,
        prev_offset,
        next_offset,
        q,
        tag,
        enabled: params.enabled,
        group,
        owner: params.owner,
        kind,
        sort: sort_key(sort),
        onboarding,
    })
}

struct MemberLite {
    id: Uuid,
    label: String,
}

fn build_row(
    t: &Target,
    metrics: Option<&DashboardMetrics>,
    owner_lookup: &HashMap<Uuid, MemberLite>,
    now: chrono::DateTime<Utc>,
) -> MonitorRow {
    let (kind, address) = describe_check(&t.check);
    let interval_label =
        humanize_duration(ChronoDuration::from_std(t.interval).unwrap_or_default());
    let class = match metrics {
        Some(m) if m.samples > 0 => status_class_for(m.last_status.as_str()),
        _ => "",
    };
    let last_status = class;
    let status_class = class;

    let last_check_label = metrics
        .and_then(|m| m.last_minute_ts)
        .and_then(|ts| chrono::DateTime::<Utc>::from_timestamp(ts, 0))
        .map(|then| relative_ago(now - then))
        .unwrap_or_else(|| "—".into());

    let next_check_label = if t.enabled {
        format!("next: {interval_label}")
    } else {
        "next: paused".into()
    };

    let uptime_30d_label = match metrics {
        Some(m) if m.samples > 0 => {
            let pct = (m.up as f64 / m.samples as f64) * 100.0;
            format!("{pct:.2}%")
        }
        _ => "—".into(),
    };

    let owner = t.owner_user_id.and_then(|id| {
        owner_lookup.get(&id).map(|m| OwnerView {
            id: m.id,
            initials: initials_from(&m.label),
            label: m.label.clone(),
            color: avatar_color(m.id),
        })
    });

    MonitorRow {
        id: t.id.to_string(),
        name: t.name.clone(),
        kind,
        address,
        interval_label,
        interval_s: t.interval.as_secs(),
        enabled: t.enabled,
        tags: t.tags.clone(),
        group_name: t.group_name.clone(),
        last_status,
        status_class,
        last_check_label,
        next_check_label,
        uptime_30d_label,
        owner,
    }
}

fn bucket_by_group(rows: Vec<MonitorRow>) -> Vec<GroupBlock> {
    let mut order: Vec<String> = Vec::new();
    let mut buckets: HashMap<String, Vec<MonitorRow>> = HashMap::new();
    for row in rows {
        let key = row
            .group_name
            .clone()
            .unwrap_or_else(|| UNGROUPED_LABEL.into());
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(row);
    }
    order
        .into_iter()
        .map(|name| {
            let rows = buckets.remove(&name).unwrap_or_default();
            let total = rows.len();
            let has_name = name != UNGROUPED_LABEL;
            let worst_status = rows.iter().map(|r| r.last_status).fold("up", worst_of);
            let avg_uptime_label = avg_uptime_label(&rows);
            GroupBlock {
                name,
                has_name,
                rows,
                total,
                worst_status,
                avg_uptime_label,
            }
        })
        .collect()
}

fn avg_uptime_label(rows: &[MonitorRow]) -> String {
    let mut sum = 0.0f64;
    let mut n = 0u32;
    for r in rows {
        let s = r.uptime_30d_label.trim_end_matches('%');
        if let Ok(v) = s.parse::<f64>() {
            sum += v;
            n += 1;
        }
    }
    if n == 0 {
        "—".into()
    } else {
        format!("{:.2}%", sum / n as f64)
    }
}

fn worst_of(acc: &'static str, next: &'static str) -> &'static str {
    let rank = |s: &str| match s {
        "error" => 4,
        "down" => 3,
        "degraded" => 2,
        "up" => 1,
        _ => 0,
    };
    if rank(next) > rank(acc) { next } else { acc }
}

fn collect_group_options(groups: &[GroupBlock], selected: &str) -> Vec<GroupOption> {
    let mut names: Vec<String> = groups
        .iter()
        .filter(|g| g.has_name)
        .map(|g| g.name.clone())
        .collect();
    names.sort();
    names.dedup();
    if !selected.is_empty() && !names.iter().any(|n| n == selected) {
        names.push(selected.to_owned());
    }
    names
        .into_iter()
        .map(|name| GroupOption {
            selected: name == selected,
            name,
        })
        .collect()
}

fn status_class_for(status: &str) -> &'static str {
    match status {
        "up" => "up",
        "down" => "down",
        "degraded" => "degraded",
        "error" => "error",
        _ => "",
    }
}

fn relative_ago(d: ChronoDuration) -> String {
    if d.num_seconds() < 0 {
        return "just now".into();
    }
    format!("{} ago", humanize_duration(d))
}

/// Splits on `@` first so emails don't all collapse to identical initials.
fn initials_from(s: &str) -> String {
    let head = s.split('@').next().unwrap_or(s);
    let mut chars = head.chars().filter(|c| c.is_alphanumeric());
    let a = chars.next().map(|c| c.to_ascii_uppercase());
    let b = chars.next().map(|c| c.to_ascii_uppercase());
    match (a, b) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) => format!("{a}"),
        _ => "—".into(),
    }
}

fn avatar_color(id: Uuid) -> String {
    let bytes = id.as_bytes();
    let hue = ((bytes[0] as u16) << 8 | bytes[1] as u16) as f32 % 360.0;
    format!("oklch(0.62 0.12 {hue:.0})")
}

fn parse_sort(raw: Option<&str>) -> TargetSort {
    match raw.unwrap_or("recent") {
        "name" => TargetSort::Name,
        "created" => TargetSort::Created,
        _ => TargetSort::RecentActivity,
    }
}

fn sort_key(s: TargetSort) -> &'static str {
    match s {
        TargetSort::RecentActivity => "recent",
        TargetSort::Name => "name",
        TargetSort::Created => "created",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, group: Option<&str>, status: &'static str, enabled: bool) -> MonitorRow {
        MonitorRow {
            id: Uuid::nil().to_string(),
            name: name.into(),
            kind: "HTTP",
            address: "https://example.com".into(),
            interval_label: "60s".into(),
            interval_s: 60,
            enabled,
            tags: vec![],
            group_name: group.map(str::to_owned),
            last_status: status,
            status_class: status_class_for(status),
            last_check_label: "3s ago".into(),
            next_check_label: "next: 60s".into(),
            uptime_30d_label: "99.94%".into(),
            owner: None,
        }
    }

    #[test]
    fn worst_of_picks_highest_severity() {
        let g = [row("a", None, "up", true), row("b", None, "down", true)];
        let s = g.iter().map(|r| r.last_status).fold("up", worst_of);
        assert_eq!(s, "down");
    }

    #[test]
    fn bucket_by_group_preserves_first_appearance_order() {
        let rows = vec![
            row("a", Some("B"), "up", true),
            row("b", Some("A"), "up", true),
            row("c", Some("B"), "up", true),
        ];
        let groups = bucket_by_group(rows);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "B");
        assert_eq!(groups[0].total, 2);
        assert_eq!(groups[1].name, "A");
    }

    #[test]
    fn bucket_renames_none_to_ungrouped() {
        let rows = vec![row("a", None, "up", true)];
        let groups = bucket_by_group(rows);
        assert_eq!(groups[0].name, "Ungrouped");
        assert!(!groups[0].has_name);
    }

    #[test]
    fn avg_uptime_label_skips_dashes() {
        let mut rs = vec![row("a", None, "up", true), row("b", None, "", true)];
        rs[1].uptime_30d_label = "—".into();
        assert_eq!(avg_uptime_label(&rs), "99.94%");
    }

    #[test]
    fn initials_split_on_at_sign() {
        assert_eq!(initials_from("ada@acme.io"), "AD");
        assert_eq!(initials_from("zoe"), "ZO");
        assert_eq!(initials_from(""), "—");
    }

    #[test]
    fn list_page_renders_onboarding_when_empty_no_filters() {
        let page = ListPage {
            active_tab: "targets",
            groups: vec![],
            total: 0,
            paused_total: 0,
            type_chips: vec![],
            owner_options: vec![],
            group_options: vec![],
            has_more: false,
            limit: 50,
            offset: 0,
            prev_offset: None,
            next_offset: None,
            q: String::new(),
            tag: String::new(),
            enabled: None,
            group: String::new(),
            owner: None,
            kind: String::new(),
            sort: "recent",
            onboarding: true,
        };
        let html = page.render().unwrap();
        assert!(html.contains("nothing to watch yet."));
        assert!(html.contains("add your first monitor"));
    }

    #[test]
    fn list_page_renders_grouped_table() {
        let g = GroupBlock {
            name: "API & Web".into(),
            has_name: true,
            total: 1,
            worst_status: "up",
            avg_uptime_label: "99.99%".into(),
            rows: vec![row("api", Some("API & Web"), "up", true)],
        };
        let page = ListPage {
            active_tab: "targets",
            groups: vec![g],
            total: 1,
            paused_total: 0,
            type_chips: vec![TypeChip {
                label: "All",
                count: 1,
                active: true,
            }],
            owner_options: vec![],
            group_options: vec![],
            has_more: false,
            limit: 50,
            offset: 0,
            prev_offset: None,
            next_offset: None,
            q: String::new(),
            tag: String::new(),
            enabled: None,
            group: String::new(),
            owner: None,
            kind: String::new(),
            sort: "recent",
            onboarding: false,
        };
        let html = page.render().unwrap();
        // Askama escapes `&` to the numeric reference `&#38;`.
        assert!(html.contains("API &#38; Web"));
        assert!(html.contains("api"));
        assert!(html.contains("99.99%"));
        // Per-row uptime is also rendered.
        assert!(html.contains("99.94%"));
    }
}
