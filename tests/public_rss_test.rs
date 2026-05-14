//! RSS 2.0 well-formedness + required-element validation for
//! `/api/public/v1/incidents.rss`.
//!
//! The hand-rolled RSS emitter in `src/public_status/source.rs` is one of the
//! few places where a quietly broken response would still parse on the client
//! side (browsers and feed readers are forgiving) — so we parse it strictly
//! with `quick-xml` and assert the required structural elements per
//! https://www.rssboard.org/rss-specification.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

use common::build_test_app_with_public_source;
use status_monitor::api::PageEnvelope;
use status_monitor::api::public_error::PublicAppError;
use status_monitor::domain::{
    ComponentHistoryResponse, IncidentSeverity, IncidentStatusPhase, OrgId, PublicIncident,
    PublicIncidentUpdate, PublicMaintenanceList, PublicStatusPage,
};
use status_monitor::public_status::{IncidentListQuery, PublicSource, source::build_rss};

const INCIDENT_TITLE: &str = "Edge proxy 5xx spike";
const INCIDENT_BODY: &str = "First report from the edge fleet — investigating.";

fn incident_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000c01").unwrap()
}

/// Two-incident source so we exercise both feed structure and per-item
/// invariants (GUID uniqueness, pubDate ordering, etc.).
struct TwoIncidentSource;

#[async_trait]
impl PublicSource for TwoIncidentSource {
    async fn page(&self, _org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError> {
        unimplemented!("not exercised by RSS test")
    }
    async fn component_history(
        &self,
        _org: OrgId,
        _id: Uuid,
        _days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError> {
        unimplemented!("not exercised by RSS test")
    }
    async fn list_incidents(
        &self,
        _org: OrgId,
        q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError> {
        let now = Utc::now();
        let items = vec![
            PublicIncident {
                id: incident_id(),
                component_id: Uuid::nil(),
                component_name: "Edge".into(),
                title: INCIDENT_TITLE.into(),
                started_at: now - chrono::Duration::minutes(30),
                ended_at: None,
                severity: IncidentSeverity::Major,
                status_phase: IncidentStatusPhase::Investigating,
                updates: vec![PublicIncidentUpdate {
                    posted_at: now - chrono::Duration::minutes(5),
                    phase: IncidentStatusPhase::Investigating,
                    message: INCIDENT_BODY.into(),
                }],
            },
            PublicIncident {
                id: Uuid::parse_str("00000000-0000-0000-0000-000000000c02").unwrap(),
                component_id: Uuid::nil(),
                component_name: "Edge".into(),
                title: "Origin TLS renewal".into(),
                started_at: now - chrono::Duration::hours(6),
                ended_at: Some(now - chrono::Duration::hours(5)),
                severity: IncidentSeverity::Minor,
                status_phase: IncidentStatusPhase::Resolved,
                updates: vec![],
            },
        ];
        Ok(PageEnvelope::new(items, 2, q.limit, q.offset))
    }
    async fn incident_by_id(
        &self,
        _org: OrgId,
        _id: Uuid,
    ) -> Result<PublicIncident, PublicAppError> {
        unimplemented!("not exercised by RSS test")
    }
    async fn maintenance(&self, _org: OrgId) -> Result<PublicMaintenanceList, PublicAppError> {
        unimplemented!("not exercised by RSS test")
    }
    async fn incidents_rss(&self, org: OrgId, base_url: &str) -> Result<String, PublicAppError> {
        let items = self
            .list_incidents(org, IncidentListQuery::default())
            .await?
            .items;
        Ok(build_rss("status-monitor", base_url, &items))
    }
}

async fn fetch_rss() -> String {
    let app = build_test_app_with_public_source(|_| {}, Arc::new(TwoIncidentSource));
    let resp = app
        .oneshot(
            Request::get("/api/public/v1/incidents.rss")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).expect("rss feed must be utf-8")
}

/// Walks the RSS document with `quick-xml` and returns the structure we
/// validate against. Strict end-name checks are on by default — malformed XML
/// aborts the test.
///
/// Each `<item>` block is captured as an ordered list of `(tag, text)` pairs
/// so per-element assertions (URI shape, RFC-822 pubDate, GUID uniqueness)
/// can inspect actual content, not just presence.
struct ParsedRss {
    rss_version: String,
    channel_elements: Vec<String>,
    item_blocks: Vec<Vec<(String, String)>>,
}

fn parse(xml: &str) -> ParsedRss {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut rss_version = String::new();
    let mut channel_elements = Vec::new();
    let mut item_blocks: Vec<Vec<(String, String)>> = Vec::new();
    let mut current_item: Option<Vec<(String, String)>> = None;
    let mut depth_channel = 0u32;
    let mut current_tag: Option<String> = None;
    let mut current_text = String::new();
    loop {
        match reader.read_event_into(&mut buf).expect("strict XML parse") {
            Event::Eof => break,
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "rss" {
                    for attr in e.attributes().with_checks(false).flatten() {
                        if attr.key.as_ref() == b"version" {
                            rss_version = String::from_utf8_lossy(&attr.value).into_owned();
                        }
                    }
                } else if name == "channel" {
                    depth_channel += 1;
                } else if name == "item" {
                    current_item = Some(Vec::new());
                } else {
                    current_tag = Some(name);
                    current_text.clear();
                }
            }
            Event::Text(e) => {
                if current_tag.is_some()
                    && let Ok(s) = e.decode()
                {
                    current_text.push_str(&s);
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "channel" {
                    depth_channel = depth_channel.saturating_sub(1);
                } else if name == "item"
                    && let Some(block) = current_item.take()
                {
                    item_blocks.push(block);
                } else if current_tag.as_deref() == Some(name.as_str()) {
                    let text = std::mem::take(&mut current_text);
                    if let Some(item) = current_item.as_mut() {
                        item.push((name.clone(), text));
                    } else if depth_channel > 0 {
                        channel_elements.push(name.clone());
                    }
                    current_tag = None;
                }
            }
            _ => {}
        }
        buf.clear();
    }
    ParsedRss {
        rss_version,
        channel_elements,
        item_blocks,
    }
}

fn item_text<'a>(block: &'a [(String, String)], tag: &str) -> Option<&'a str> {
    block
        .iter()
        .find(|(t, _)| t == tag)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn rss_feed_parses_strictly_as_xml() {
    let xml = fetch_rss().await;
    // quick-xml.read_event panics inside `parse` if the document is not
    // well-formed; this assertion is mostly a no-op once we get here, but
    // double-checks we received non-empty bytes.
    assert!(xml.contains("<?xml"));
    let _ = parse(&xml);
}

#[tokio::test]
async fn rss_root_declares_version_2_0() {
    let xml = fetch_rss().await;
    let parsed = parse(&xml);
    assert_eq!(parsed.rss_version, "2.0", "rss version must be 2.0");
}

#[tokio::test]
async fn rss_channel_has_required_children() {
    // RSS 2.0 §"Required channel elements": title, link, description.
    // We also emit lastBuildDate (recommended) — assert present so a future
    // refactor doesn't silently drop it.
    let xml = fetch_rss().await;
    let parsed = parse(&xml);
    for required in ["title", "link", "description", "lastBuildDate"] {
        assert!(
            parsed.channel_elements.iter().any(|e| e == required),
            "channel missing <{required}>: saw {:?}",
            parsed.channel_elements
        );
    }
}

#[tokio::test]
async fn rss_item_has_required_children() {
    // RSS 2.0 says an item must contain *at least* title OR description, plus
    // a guid for stability. We emit all five (title, link, guid, pubDate,
    // description) and assert each so the feed renders sensibly in readers.
    let xml = fetch_rss().await;
    let parsed = parse(&xml);
    assert!(
        !parsed.item_blocks.is_empty(),
        "feed must contain at least one <item>"
    );
    for block in &parsed.item_blocks {
        for required in ["title", "link", "guid", "pubDate", "description"] {
            assert!(
                block.iter().any(|(t, _)| t == required),
                "<item> missing <{required}>: saw {block:?}"
            );
        }
    }
}

#[tokio::test]
async fn rss_item_pubdate_parses_as_rfc822() {
    // Feed readers reject items whose `<pubDate>` isn't a valid RFC-822 date.
    // The hand-rolled builder uses `DateTime::to_rfc2822()`; if a refactor
    // ever swaps that for RFC-3339, every feed reader breaks silently.
    let xml = fetch_rss().await;
    let parsed = parse(&xml);
    for block in &parsed.item_blocks {
        let raw = item_text(block, "pubDate").expect("pubDate present");
        DateTime::parse_from_rfc2822(raw).unwrap_or_else(|e| {
            panic!("pubDate '{raw}' is not RFC-822: {e}");
        });
    }
}

#[tokio::test]
async fn rss_item_link_parses_as_absolute_uri() {
    let xml = fetch_rss().await;
    let parsed = parse(&xml);
    for block in &parsed.item_blocks {
        let raw = item_text(block, "link").expect("link present");
        let u = Url::parse(raw).unwrap_or_else(|e| panic!("link '{raw}' invalid: {e}"));
        assert!(
            u.scheme() == "http" || u.scheme() == "https",
            "link must be http(s): {raw}"
        );
    }
}

#[tokio::test]
async fn rss_item_guids_are_unique() {
    // RSS 2.0 §"guid" — the value must be unique across the feed.
    let xml = fetch_rss().await;
    let parsed = parse(&xml);
    let mut seen: std::collections::HashSet<&str> = Default::default();
    for block in &parsed.item_blocks {
        let g = item_text(block, "guid").expect("guid present");
        assert!(seen.insert(g), "duplicate guid: {g}");
    }
}
