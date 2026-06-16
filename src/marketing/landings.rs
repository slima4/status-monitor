//! SEO landing pages — use-case and comparison pages on the marketing
//! host (`/status-page-for-saas`, `/vs/uptimerobot`, …). Same slice-driven
//! shape as [`super::legal`]: one `LANDINGS` table feeds the router mount,
//! the render cache, and the sitemap, so a new page is one entry.
//!
//! Copy is authored factual about Uptimepage. Comparison pages state what
//! Uptimepage offers and stay neutral about the named competitor — no
//! third-party feature claims to keep current. Vet any competitor-specific
//! copy before adding it.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use askama::Template;
use askama_web::WebTemplate;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::Response;
use axum::routing::get;

use super::config::{BRAND, MarketingCfg};
use super::pages::{CachedRender, cached_render, serve_cached};
use super::seo::{JsonLd, OpenGraph, json_ld_breadcrumb};
use crate::web::filters;

const LANDING_CACHE_CONTROL: HeaderValue =
    HeaderValue::from_static("public, max-age=300, stale-while-revalidate=86400");

/// One row in a landing page's "what you get" table.
pub struct Feature {
    pub label: &'static str,
    pub value: &'static str,
}

/// One prose block in the page body.
pub struct Section {
    pub heading: &'static str,
    pub body: &'static str,
}

/// A use-case or comparison landing page. Comparison and use-case pages
/// share one shape — the only difference is copy — so there is no kind
/// discriminant to keep in lockstep.
pub struct Landing {
    pub path: &'static str,
    /// `<title>` and OpenGraph title (brand suffix added at render).
    pub title: &'static str,
    pub eyebrow: &'static str,
    pub h1: &'static str,
    pub meta_description: &'static str,
    pub lede: &'static str,
    pub features: &'static [Feature],
    pub sections: &'static [Section],
    pub cta: &'static str,
}

/// Single source of truth: router mount, render cache, and sitemap all
/// iterate this slice. Add a page → one entry.
pub const LANDINGS: &[Landing] = &[
    Landing {
        path: "/status-page-for-saas",
        title: "Status Page & Uptime Monitoring for SaaS",
        eyebrow: "for saas teams",
        h1: "A status page your SaaS customers actually trust",
        meta_description: "Public status pages and 60-second uptime monitoring for SaaS teams. HTTP, TCP, DNS, TLS checks, Slack, email and webhook alerts, 90-day history. Free to start.",
        lede: "Monitor every dependency, open incidents automatically, and show customers a branded status page on your own subdomain — without standing up a status tool of your own.",
        features: &[
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Check types",
                value: "HTTP, TCP, DNS, TLS",
            },
            Feature {
                label: "Alert channels",
                value: "Slack, email, webhook",
            },
            Feature {
                label: "Public history",
                value: "90 days",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "monitor the whole stack",
                body: "Your API, your database, your payment provider, your mail sender. HTTP, TCP, DNS and TLS checks every minute, each with its own expectations and its own alert channels.",
            },
            Section {
                heading: "tell customers before they tell you",
                body: "A down monitor opens an incident automatically and posts it to your public page. Add a human note and your customers watch the fix land in real time.",
            },
            Section {
                heading: "alerts that don’t cry wolf",
                body: "Per-monitor channels, dedupe and flap-suppression mean a 60-second blip never pages on-call at 3 a.m. The signal stays honest.",
            },
        ],
        cta: "Start free with GitHub",
    },
    Landing {
        path: "/status-page-for-agencies",
        title: "Status Pages for Agencies & Client Sites",
        eyebrow: "for agencies",
        h1: "One account. A branded status page for every client.",
        meta_description: "Monitor every client site and give each a branded status page from one account. 60s checks, Slack, email and webhook alerts. Free to start.",
        lede: "Watch all your clients’ sites from a single dashboard and hand each one a status URL on its own subdomain — no per-client tool, no per-client invoice.",
        features: &[
            Feature {
                label: "Clients per account",
                value: "unlimited pages",
            },
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Branding",
                value: "logo + colour per page",
            },
            Feature {
                label: "Public history",
                value: "90 days",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "every client, one tab",
                body: "Add each client site as a monitor, group them, and see the whole roster’s health at a glance. Switch a monitor public and that client gets a branded page.",
            },
            Section {
                heading: "look like the shop they hired",
                body: "Brand colour and logo per page, a 90-day timeline, live incidents and scheduled-maintenance windows. Polished from day one.",
            },
            Section {
                heading: "bill it however you like",
                body: "One account covers many pages, so there is no metered per-monitor pricing to pass through to clients while you are getting started.",
            },
        ],
        cta: "Start free with GitHub",
    },
    Landing {
        path: "/vs/uptimerobot",
        title: "An UptimeRobot Alternative with Built-in Status Pages",
        eyebrow: "switching monitors",
        h1: "Looking for an UptimeRobot alternative?",
        meta_description: "Comparing uptime monitors? Uptimepage pairs 60s HTTP, TCP, DNS and TLS checks with branded status pages and Slack, email and webhook alerts. Free to start.",
        lede: "If you are weighing your options, here is what Uptimepage gives you out of the box. Everything below is on the free tier — no card, sign in with GitHub.",
        features: &[
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Status pages",
                value: "built in, branded",
            },
            Feature {
                label: "Check types",
                value: "HTTP, TCP, DNS, TLS",
            },
            Feature {
                label: "Alerts",
                value: "Slack, email, webhook",
            },
            Feature {
                label: "Data export",
                value: "JSON, RSS, SVG badge",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "monitoring and status page in one",
                body: "Checks and a public status page are the same product here, not an add-on. Flip any monitor public and it lands on your subdomain with a 90-day history.",
            },
            Section {
                heading: "checks that explain themselves",
                body: "HTTP, TCP, DNS and TLS, every minute. When something is slow, the timing is split across DNS, connect, TLS and time-to-first-byte — so you see why, not just that.",
            },
            Section {
                heading: "alerts tuned for humans",
                body: "Per-monitor Slack, email and webhook channels with dedupe and flap-suppression, so a brief blip doesn’t page anyone.",
            },
        ],
        cta: "Start free with GitHub",
    },
];

#[derive(Template, WebTemplate)]
#[template(path = "marketing/landing_page.html")]
struct LandingDoc {
    title: String,
    eyebrow: &'static str,
    h1: &'static str,
    lede: &'static str,
    features: &'static [Feature],
    sections: &'static [Section],
    cta: &'static str,
    canonical_url: String,
    og: OpenGraph,
    breadcrumb_json_ld: JsonLd,
    app_url: String,
    version: &'static str,
}

static RENDERED: OnceLock<HashMap<&'static str, CachedRender>> = OnceLock::new();

fn render_all(cfg: &MarketingCfg) -> HashMap<&'static str, CachedRender> {
    LANDINGS
        .iter()
        .map(|l| {
            let canonical_url = format!("{}{}", cfg.canonical_origin, l.path);
            let title = format!("{} — {BRAND}", l.title);
            let mut og = OpenGraph::default_for(&title, &canonical_url);
            og.description = l.meta_description.to_string();
            let doc = LandingDoc {
                title,
                eyebrow: l.eyebrow,
                h1: l.h1,
                lede: l.lede,
                features: l.features,
                sections: l.sections,
                cta: l.cta,
                canonical_url,
                og,
                breadcrumb_json_ld: json_ld_breadcrumb(&cfg.canonical_origin, l.h1, l.path),
                app_url: cfg.app_url.clone(),
                version: env!("CARGO_PKG_VERSION"),
            };
            let body = doc
                .render()
                .unwrap_or_else(|e| format!("<!-- landing render failed: {e} -->"));
            (l.path, cached_render(body))
        })
        .collect()
}

/// Warm the per-page render cache at boot so the first hit doesn't pay
/// askama + SHA-256.
pub(crate) fn warm(cfg: &MarketingCfg) {
    RENDERED.get_or_init(|| render_all(cfg));
}

async fn serve(
    State(cfg): State<Arc<MarketingCfg>>,
    headers: HeaderMap,
    landing: &'static Landing,
) -> Response {
    let map = RENDERED.get_or_init(|| render_all(&cfg));
    let cached = map.get(landing.path).expect("LANDINGS drives render");
    serve_cached(&headers, cached, &LANDING_CACHE_CONTROL)
}

/// Mount every entry in [`LANDINGS`]. One source of truth — no per-page
/// handler, no `match` arm.
pub fn mount(router: Router<Arc<MarketingCfg>>) -> Router<Arc<MarketingCfg>> {
    let mut r = router;
    for landing in LANDINGS {
        r = r.route(
            landing.path,
            get(move |state, headers| serve(state, headers, landing)),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_unique_and_rooted() {
        let mut seen = std::collections::HashSet::new();
        for l in LANDINGS {
            assert!(l.path.starts_with('/'), "{} must be absolute", l.path);
            assert!(seen.insert(l.path), "duplicate path {}", l.path);
        }
    }

    #[test]
    fn every_landing_has_seo_essentials() {
        for l in LANDINGS {
            assert!(!l.title.is_empty(), "{} missing title", l.path);
            assert!(!l.h1.is_empty(), "{} missing h1", l.path);
            assert!(
                l.meta_description.len() <= 160,
                "{} meta description {} chars > 160",
                l.path,
                l.meta_description.len()
            );
            assert!(!l.features.is_empty(), "{} missing features", l.path);
            assert!(!l.sections.is_empty(), "{} missing sections", l.path);
        }
    }
}
