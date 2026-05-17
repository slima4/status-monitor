use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Query, State};
use chrono::{Duration, Utc};
use futures::future::join_all;
use serde::Deserialize;

use crate::app::AppState;
use crate::domain::{CheckResult, Target};
use crate::storage::{TargetFilter, TimeRange};
use crate::web::AuthedBrowser;
use crate::web::assets::filters;
use crate::web::error::WebResult;
use crate::web::views::{describe_check, fmt_ts};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const LAST_RESULT_WINDOW_DAYS: i64 = 7;

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

pub struct TargetRow {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub interval_s: u64,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub last_status: &'static str,
    pub last_at: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/list.html")]
pub struct ListPage {
    pub active_tab: &'static str,
    pub rows: Vec<TargetRow>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
    pub prev_offset: Option<usize>,
    pub next_offset: Option<usize>,
    pub q: String,
    pub tag: String,
    pub enabled: Option<bool>,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/partials/list_body.html")]
pub struct ListBodyPartial {
    pub rows: Vec<TargetRow>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
    pub prev_offset: Option<usize>,
    pub next_offset: Option<usize>,
    pub q: String,
    pub tag: String,
    pub enabled: Option<bool>,
}

pub async fn index(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> WebResult<ListPage> {
    let (rows, total, limit, offset) = fetch_rows(&state, &params).await?;
    let (prev_offset, next_offset) = pagination_offsets(offset, limit, rows.len(), total);
    Ok(ListPage {
        active_tab: "targets",
        rows,
        total,
        limit,
        offset,
        prev_offset,
        next_offset,
        q: params.q.unwrap_or_default(),
        tag: params.tag.unwrap_or_default(),
        enabled: params.enabled,
    })
}

pub async fn list_partial(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> WebResult<ListBodyPartial> {
    let (rows, total, limit, offset) = fetch_rows(&state, &params).await?;
    let (prev_offset, next_offset) = pagination_offsets(offset, limit, rows.len(), total);
    Ok(ListBodyPartial {
        rows,
        total,
        limit,
        offset,
        prev_offset,
        next_offset,
        q: params.q.unwrap_or_default(),
        tag: params.tag.unwrap_or_default(),
        enabled: params.enabled,
    })
}

fn pagination_offsets(
    offset: usize,
    limit: usize,
    page_len: usize,
    total: u64,
) -> (Option<usize>, Option<usize>) {
    let prev = (offset > 0).then(|| offset.saturating_sub(limit));
    let next = ((offset + page_len) < total as usize).then_some(offset + limit);
    (prev, next)
}

async fn fetch_rows(
    state: &AppState,
    params: &ListParams,
) -> Result<(Vec<TargetRow>, u64, usize, usize), crate::error::AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let filter = TargetFilter {
        limit: Some(limit),
        offset,
        tag: params.tag.clone().filter(|s| !s.is_empty()),
        enabled: params.enabled,
    };

    let (targets, total) = tokio::try_join!(
        state.target_store.list(filter.clone()),
        state.target_store.count(filter),
    )?;

    let now = Utc::now();
    let range = TimeRange {
        from: now - Duration::days(LAST_RESULT_WINDOW_DAYS),
        to: now,
    };
    let last_results = join_all(targets.iter().map(|t| {
        let store = state.results_store.clone();
        let id = t.id;
        async move {
            store
                .list_results(id, range, 1, 0)
                .await
                .ok()
                .and_then(|mut v| v.pop())
        }
    }))
    .await;

    let rows: Vec<TargetRow> = targets
        .into_iter()
        .zip(last_results)
        .map(|(t, last)| make_row(t, last))
        .collect();

    Ok((rows, total, limit, offset))
}

fn make_row(t: Target, last: Option<CheckResult>) -> TargetRow {
    let (kind, address) = describe_check(&t.check);
    TargetRow {
        id: t.id.to_string(),
        name: t.name,
        kind,
        address,
        interval_s: t.interval.as_secs(),
        enabled: t.enabled,
        tags: t.tags,
        last_status: last.as_ref().map(|r| r.status.as_str()).unwrap_or(""),
        last_at: last.map(|r| fmt_ts(r.timestamp)).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(name: &str) -> TargetRow {
        TargetRow {
            id: "00000000-0000-0000-0000-000000000001".into(),
            name: name.into(),
            kind: "HTTP",
            address: "https://example.com".into(),
            interval_s: 60,
            enabled: true,
            tags: vec!["prod".into()],
            last_status: "up",
            last_at: "2026-05-13T12:00:00Z".into(),
        }
    }

    #[test]
    fn list_page_renders_table_chrome() {
        let page = ListPage {
            active_tab: "targets",
            rows: vec![sample_row("api")],
            total: 1,
            limit: 50,
            offset: 0,
            q: String::new(),
            tag: String::new(),
            enabled: None,
            prev_offset: None,
            next_offset: None,
        };
        let html = page.render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Targets"));
        assert!(html.contains("api"));
        assert!(html.contains(r#"hx-get="/web/targets/list""#));
    }

    #[test]
    fn list_partial_renders_tbody_only() {
        let partial = ListBodyPartial {
            rows: vec![sample_row("api"), sample_row("worker")],
            total: 100,
            limit: 50,
            offset: 0,
            q: String::new(),
            tag: String::new(),
            enabled: Some(true),
            prev_offset: None,
            next_offset: Some(50),
        };
        let html = partial.render().unwrap();
        assert!(html.contains(r#"<tbody id="target-rows">"#));
        assert!(!html.contains("<!doctype html>"));
        assert!(html.contains("api"));
        assert!(html.contains("worker"));
        assert!(html.contains("enabled=true"));
    }

    #[test]
    fn empty_state_renders_message() {
        let partial = ListBodyPartial {
            rows: vec![],
            total: 0,
            limit: 50,
            offset: 0,
            q: String::new(),
            tag: String::new(),
            enabled: None,
            prev_offset: None,
            next_offset: None,
        };
        let html = partial.render().unwrap();
        assert!(html.contains("No targets match"));
    }
}
