//! SEO landing pages — use-case and comparison pages on the marketing
//! host (`/status-page-for-saas`, `/vs/uptimerobot`, …). Same slice-driven
//! shape as [`super::legal`]: one `LANDINGS` table feeds the router mount,
//! the render cache, and the sitemap, so a new page is one entry.
//!
//! Copy is authored factual about Uptimepage. Most comparison pages state
//! what Uptimepage offers and stay neutral about the named competitor, with
//! no third-party feature claims to keep current. The one exception is the
//! head-to-head matrix page (`/vs/self-hosted-status-pages`), which carries
//! dated, source-verified competitor facts via `page_matrix`; refresh those
//! against each project's repo when they drift. Vet any competitor-specific
//! copy before adding it.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use askama::Template;
use askama_web::WebTemplate;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::{Redirect, Response};
use axum::routing::get;

use super::config::{BRAND, MarketingCfg, TERRAFORM_URL};
use super::pages::{CachedRender, cached_render, serve_cached};
use super::seo::{
    JsonLd, OpenGraph, json_ld_breadcrumb, json_ld_faqpage, json_ld_software_application,
    json_ld_webpage,
};
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

pub struct ResourceLink {
    pub label: &'static str,
    pub href: &'static str,
}

pub struct CodeSample {
    pub caption: &'static str,
    pub body: &'static str,
}

/// One row of a head-to-head comparison matrix: a label plus one
/// `(text, tone)` cell per column. `tone` is a `.cmp` cell class
/// (`""`, `"yes"`, `"no"`, `"part"`) that colours the value.
pub struct MatrixRow {
    pub label: &'static str,
    pub cells: &'static [(&'static str, &'static str)],
}

/// A factual, dated comparison matrix. Only the head-to-head page carries
/// one, so it is looked up by path (like the FAQs) rather than stored on
/// every `Landing`. Keep `notes` verifiable and the last one dated.
pub struct Matrix {
    pub heading: &'static str,
    pub columns: &'static [&'static str],
    pub rows: &'static [MatrixRow],
    pub notes: &'static [&'static str],
}

impl Matrix {
    /// Index of the highlighted Uptimepage column, wherever it sits: first
    /// on `/vs/` pages, last on `/compare/` face-offs.
    pub fn us_col(&self) -> usize {
        self.columns
            .iter()
            .position(|c| *c == BRAND)
            .expect("matrix missing Uptimepage column")
    }
}

/// A use-case or comparison landing page. Comparison and use-case pages
/// share one shape — the only difference is copy — so there is no kind
/// discriminant to keep in lockstep.
pub struct Landing {
    pub path: &'static str,
    pub created: &'static str,
    pub lastmod: &'static str,
    /// `<title>` and OpenGraph title (brand suffix added at render).
    pub title: &'static str,
    pub eyebrow: &'static str,
    pub h1: &'static str,
    pub meta_description: &'static str,
    pub lede: &'static str,
    pub features: &'static [Feature],
    pub sections: &'static [Section],
    pub code: Option<CodeSample>,
    pub resources: &'static [ResourceLink],
    pub cta: &'static str,
}

/// Single source of truth: router mount, render cache, and sitemap all
/// iterate this slice. Add a page → one entry.
pub const LANDINGS: &[Landing] = &[
    Landing {
        path: "/status-page-for-saas",
        created: "2026-06-16",
        lastmod: "2026-06-21",
        title: "Status Page & Uptime Monitoring for SaaS",
        eyebrow: "for saas teams",
        h1: "A status page your SaaS customers actually trust",
        meta_description: "Public status pages and 60-second uptime monitoring for SaaS teams. HTTP, TCP, DNS, TLS checks, Slack, email and webhook alerts, 90-day history. Free to start.",
        lede: "Monitor every dependency, open incidents automatically, and show customers a branded status page on your own subdomain, without standing up a status tool of your own.",
        features: &[
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Check types",
                value: "HTTP, TCP, DNS, TLS, ping",
            },
            Feature {
                label: "Alert channels",
                value: "Slack, Telegram, PagerDuty, SMS + more",
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
                body: "Your API, your database, your payment provider, your mail sender. HTTP, TCP, DNS, TLS and ping checks every minute, each with its own expectations and its own alert channels.",
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
        code: None,
        resources: &[
            ResourceLink {
                label: "Free pricing",
                href: "/pricing",
            },
            ResourceLink {
                label: "Uptime SLA calculator",
                href: "/tools/uptime-sla-calculator",
            },
            ResourceLink {
                label: "Versus Statuspage",
                href: "/vs/statuspage",
            },
            ResourceLink {
                label: "White-label pages",
                href: "/white-label-uptime-monitoring",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/status-page-for-agencies",
        created: "2026-06-16",
        lastmod: "2026-06-21",
        title: "Status Pages for Agencies & Client Sites",
        eyebrow: "for agencies",
        h1: "One account. A branded status page for every client.",
        meta_description: "Monitor every client site and give each a branded status page from one account. 60s checks, Slack, email and webhook alerts. Free to start.",
        lede: "Watch all your clients’ sites from a single dashboard and hand each one a status URL on its own subdomain, with no per-client tool and no per-client invoice.",
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
        code: None,
        resources: &[
            ResourceLink {
                label: "Free pricing",
                href: "/pricing",
            },
            ResourceLink {
                label: "For SaaS teams",
                href: "/status-page-for-saas",
            },
            ResourceLink {
                label: "White-label pages",
                href: "/white-label-uptime-monitoring",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/open-source-status-page",
        created: "2026-06-20",
        lastmod: "2026-07-02",
        title: "Open-Source Status Page, Monitoring Built In",
        eyebrow: "open source",
        h1: "An open-source status page",
        meta_description: "An open-source status page with built-in uptime and website monitoring. Branded pages, subscribers, incidents, maintenance. AGPL, free, self-host or hosted.",
        lede: "Uptimepage is an AGPL status page with website and uptime monitoring built in. Publish a branded page on your own subdomain, let customers subscribe, and run it yourself or on the free hosted tier.",
        features: &[
            Feature {
                label: "License",
                value: "AGPL, self-host",
            },
            Feature {
                label: "Status page",
                value: "branded, subscribers",
            },
            Feature {
                label: "Monitoring",
                value: "built in",
            },
            Feature {
                label: "Subscribers",
                value: "email + webhook",
            },
            Feature {
                label: "Stack",
                value: "one binary + Postgres + ClickHouse",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "a status page, not a toy",
                body: "Branded public pages on your own domain, a 90-day history strip, incident timelines and scheduled maintenance windows. Customers see the truth without you standing up a second tool.",
            },
            Section {
                heading: "monitoring is built in",
                body: "Incidents open automatically from real HTTP, TCP, DNS, TLS and ping checks and flow straight onto the page. There is no separate monitor to wire up and keep in sync.",
            },
            Section {
                heading: "open source, your way",
                body: "The source is AGPL. Run docker compose up with Postgres and ClickHouse on your own servers, or start on the free hosted tier. The API and Terraform provider are the same either way.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Status page for SaaS",
                href: "/status-page-for-saas",
            },
            ResourceLink {
                label: "vs Statuspage",
                href: "/vs/statuspage",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
            ResourceLink {
                label: "vs self-hosted monitors",
                href: "/vs/self-hosted-monitoring",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/open-source-uptime-monitoring",
        created: "2026-07-11",
        lastmod: "2026-07-11",
        title: "Open-Source Uptime Monitoring, Self-Hosted",
        eyebrow: "open source",
        h1: "An open-source uptime monitor you run yourself",
        meta_description: "An open-source uptime monitor you can self-host: HTTP, TCP, DNS, TLS and ping checks from many regions, automatic incidents and a status page. AGPL, free.",
        lede: "Uptimepage is an AGPL uptime monitor with incidents and a status page built in, written in Rust. Run the single static binary on your own servers, or start free on the hosted tier. HTTP, TCP, DNS, TLS, ping and cron-heartbeat checks from as many regions as you run.",
        features: &[],
        sections: &[
            Section {
                heading: "written in rust",
                body: "The whole product is one statically linked Rust binary. That means a small memory footprint, no runtime or interpreter to install, and probes fast enough to check every 60 seconds from many regions without a heavy host. Memory safety without a garbage collector is why teams keep rewriting their infrastructure in Rust, and it is what keeps the monitor predictable under load.",
            },
            Section {
                heading: "one binary, not a stack to babysit",
                body: "That Rust binary needs only Postgres for config and ClickHouse for the time-series. docker compose up brings it up with migrations applied on boot. No Kubernetes, no queue, nothing else to operate.",
            },
            Section {
                heading: "for developers",
                body: "Declare monitors, status pages and channels in Terraform and review changes in a pull request. A full REST API and an MCP server mirror the dashboard, authenticated with scoped, org-bound tokens you can narrow to a single job.",
            },
            Section {
                heading: "for devops and sre",
                body: "Run regional probe agents on your own servers and fold their results into each monitor per region. Failing checks open incidents automatically and route to Slack, Telegram, PagerDuty or SMS, with dedupe and flap-suppression so a 60-second blip never pages at 3 a.m.",
            },
            Section {
                heading: "for the company",
                body: "A branded public status page with confirmed email and webhook subscribers comes in the same binary, so customers see the truth without a second tool. Self-host to keep every check result, incident and subscriber inside your own environment.",
            },
            Section {
                heading: "open source, your way",
                body: "The source is AGPL: read it, run it, modify it. Self-host on your own infrastructure, or start on the free hosted tier and keep the self-hosted exit. The API and Terraform provider are identical either way.",
            },
        ],
        code: Some(CodeSample {
            caption: "Run it yourself",
            body: r#"git clone https://github.com/uptimepage/uptimepage
cd uptimepage
docker compose up -d"#,
        }),
        resources: &[
            ResourceLink {
                label: "Self-hosted status page",
                href: "/self-hosted-status-page",
            },
            ResourceLink {
                label: "For developers",
                href: "/uptime-monitoring-for-developers",
            },
            ResourceLink {
                label: "vs Uptime Kuma",
                href: "/vs/uptime-kuma",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/self-hosted-status-page",
        created: "2026-06-20",
        lastmod: "2026-07-02",
        title: "Self-Hosted Status Page & Uptime Monitoring",
        eyebrow: "run it yourself",
        h1: "A self-hosted status page and uptime monitor",
        meta_description: "Self-hosted uptime monitoring and status pages in one AGPL binary. docker compose up with Postgres and ClickHouse. Multi-region, free, your data on your boxes.",
        lede: "Run the whole thing yourself: monitoring, incidents and a public status page in one self-contained binary. docker compose up and you are live, with your data on your own infrastructure.",
        features: &[
            Feature {
                label: "License",
                value: "AGPL, self-host",
            },
            Feature {
                label: "Deploy",
                value: "docker compose up",
            },
            Feature {
                label: "Stack",
                value: "one binary + Postgres + ClickHouse",
            },
            Feature {
                label: "Probes",
                value: "regional agents you run",
            },
            Feature {
                label: "As code",
                value: "Terraform + REST + MCP",
            },
            Feature {
                label: "Price to start",
                value: "free, AGPL",
            },
        ],
        sections: &[
            Section {
                heading: "up with one command",
                body: "One self-contained binary, Postgres for config and ClickHouse for the time-series. docker compose up brings the whole stack up with migrations applied on boot. Nothing else to operate, no Kubernetes.",
            },
            Section {
                heading: "your data on your boxes",
                body: "Run it on your own infrastructure, in your own region, behind your own network. Nothing leaves your environment, and the public status page serves straight from it.",
            },
            Section {
                heading: "probes you own",
                body: "Run regional probe agents wherever your users are and fold their results into each monitor per region. The same Terraform config and API calls run against hosted and self-hosted alike.",
            },
        ],
        code: Some(CodeSample {
            caption: "Bring the stack up",
            body: r#"git clone https://github.com/uptimepage/uptimepage
cd uptimepage
docker compose up -d"#,
        }),
        resources: &[
            ResourceLink {
                label: "vs Uptime Kuma",
                href: "/vs/uptime-kuma",
            },
            ResourceLink {
                label: "Monitoring as code",
                href: "/automation",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
            ResourceLink {
                label: "vs Upptime, Cachet, Statping",
                href: "/vs/self-hosted-status-pages",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/white-label-uptime-monitoring",
        created: "2026-07-01",
        lastmod: "2026-07-01",
        title: "White-Label Uptime Monitoring & Status Pages",
        eyebrow: "white label",
        h1: "White-label uptime monitoring for your brand",
        meta_description: "White-label uptime monitoring and branded status pages for agencies and resellers. Your logo, colours and subdomain per client. Free to start, no card.",
        lede: "Put your own brand on the monitoring. Give every client a branded status page on your own subdomain with your logo and colours, all from one account. On Pro or a self-hosted instance the vendor badge comes off entirely, so your clients only ever see your name.",
        features: &[
            Feature {
                label: "Branding",
                value: "logo + colours per page",
            },
            Feature {
                label: "Domain",
                value: "branded subdomain per client",
            },
            Feature {
                label: "Clients",
                value: "unlimited pages",
            },
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "As code",
                value: "Terraform + REST + MCP",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "your brand, not ours",
                body: "Each status page carries your logo and colours on your own subdomain, so it reads as yours from the first visit. On Pro or a self-hosted instance the powered-by badge comes off too, and the tool behind it disappears completely.",
            },
            Section {
                heading: "a page per client, one account",
                body: "Add every client as a monitor, group them, and hand each one a branded page. No per-client tool to stand up and no per-client invoice to pass on.",
            },
            Section {
                heading: "own the whole thing",
                body: "Self-host the AGPL binary and no outside name touches your stack, or start on the free hosted tier. The API and Terraform provider are identical either way.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Status pages for agencies",
                href: "/status-page-for-agencies",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/uptime-monitoring-for-developers",
        created: "2026-07-01",
        lastmod: "2026-07-02",
        title: "Uptime Monitoring for Developers, as Code",
        eyebrow: "for developers",
        h1: "Uptime monitoring built for developers",
        meta_description: "Uptime monitoring for developers: define monitors as code with a Terraform provider, REST API and MCP. HTTP, TCP, DNS, TLS checks. Free to start, no card.",
        lede: "Define your monitors the way you define the rest of your infrastructure: in code, reviewed in a pull request. A Terraform provider, a full REST API and an MCP server, plus a status page your users can trust. Run the single binary yourself or start free on the hosted tier, no card.",
        features: &[
            Feature {
                label: "As code",
                value: "Terraform + REST + MCP",
            },
            Feature {
                label: "Checks",
                value: "HTTP, TCP, DNS, TLS, ping",
            },
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Self-host",
                value: "one binary, AGPL",
            },
            Feature {
                label: "Probes",
                value: "multi-region, run your own",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "monitors as code",
                body: "Declare monitors, status pages and notification channels in HCL with the official Terraform provider. Run `terraform plan` on every pull request so a reviewer sees the diff before it ships, and roll back a bad check with a revert.",
            },
            Section {
                heading: "an API that means it",
                body: "A full REST API covers everything the dashboard does, authenticated with scoped, org-bound tokens you can narrow to exactly one job. Script onboarding, wire checks into CI, or build your own tooling on top.",
            },
            Section {
                heading: "query it from your assistant",
                body: "An MCP server lets an LLM client read your monitoring and take fenced, audited actions. Ask what is broken and since when in plain language, answered from the same config that lives in your repo.",
            },
            Section {
                heading: "probes where your users are",
                body: "Run regional probe agents on your own servers and check from where your customers actually are. Each agent authenticates with a scoped, org-bound token.",
            },
        ],
        code: Some(CodeSample {
            caption: "Declare a monitor in Terraform",
            body: r#"resource "uptimepage_target" "api" {
  name     = "api prod"
  interval = 60

  check = {
    type = "http"
    http = {
      url = "https://example.com/healthz"
      expected_status = {
        kind  = "exact"
        exact = 200
      }
    }
  }
}"#,
        }),
        resources: &[
            ResourceLink {
                label: "Monitoring as code",
                href: "/automation",
            },
            ResourceLink {
                label: "Terraform provider",
                href: "/terraform-uptime-monitoring",
            },
            ResourceLink {
                label: "Uptime SLA calculator",
                href: "/tools/uptime-sla-calculator",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
            ResourceLink {
                label: "Built in Rust",
                href: "/blog/building-an-uptime-monitor-in-rust",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/uptimerobot",
        created: "2026-06-16",
        lastmod: "2026-07-02",
        title: "An UptimeRobot Alternative with Status Pages",
        eyebrow: "switching monitors",
        h1: "Looking for an UptimeRobot alternative?",
        meta_description: "Comparing uptime monitors? Uptimepage pairs 60s HTTP, TCP, DNS and TLS checks with branded status pages and Slack, email and webhook alerts. Free to start.",
        lede: "If you are comparing monitors, here is what Uptimepage gives you by default. Everything below is on the free tier, no card.",
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
                value: "HTTP, TCP, DNS, TLS, ping",
            },
            Feature {
                label: "Alerts",
                value: "Slack, Telegram, PagerDuty, SMS + more",
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
                body: "HTTP, TCP, DNS, TLS and ping, every minute. When something is slow, the timing is split across DNS, connect, TLS and time-to-first-byte, so you see why, not just that.",
            },
            Section {
                heading: "alerts tuned for humans",
                body: "Per-monitor Slack, Telegram, PagerDuty, SMS, email and webhook channels with dedupe and flap-suppression, so a brief outage doesn’t page anyone.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Free pricing",
                href: "/pricing",
            },
            ResourceLink {
                label: "vs Pingdom",
                href: "/vs/pingdom",
            },
            ResourceLink {
                label: "Status pages for SaaS",
                href: "/status-page-for-saas",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/statuspage",
        created: "2026-06-19",
        lastmod: "2026-07-02",
        title: "Statuspage Alternative with Monitoring Built In",
        eyebrow: "switching status pages",
        h1: "A Statuspage alternative with monitoring built in",
        meta_description: "Uptimepage pairs a branded public status page with uptime monitoring in one product: 60s checks, email and webhook subscribers, incidents. Free to start.",
        lede: "Here the status page and the monitoring behind it are the same product. Flip any monitor public and customers get a branded page on your own subdomain, all of it on the free tier.",
        features: &[
            Feature {
                label: "Status page",
                value: "built in, branded subdomain",
            },
            Feature {
                label: "Monitoring",
                value: "included, every 60s",
            },
            Feature {
                label: "Subscribers",
                value: "email + webhook",
            },
            Feature {
                label: "Incidents",
                value: "auto-open + scheduled maintenance",
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
                heading: "the page and the monitoring are one product",
                body: "You don’t wire a separate monitor up to the page. A down check opens an incident and posts it to your public status page automatically, with a 90-day history and per-component status.",
            },
            Section {
                heading: "keep customers informed",
                body: "Visitors subscribe for email or webhook updates and hear the moment an incident opens, updates, or resolves. Schedule maintenance windows ahead of time so planned work never reads as an outage.",
            },
            Section {
                heading: "branded, on your own subdomain",
                body: "Logo, colour, and a status URL on your subdomain. The page serves HTML for people and JSON plus RSS for machines, and stays up even when the backend behind it is failing.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Status pages for SaaS",
                href: "/status-page-for-saas",
            },
            ResourceLink {
                label: "Open-source status page",
                href: "/open-source-status-page",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/better-stack",
        created: "2026-06-19",
        lastmod: "2026-07-02",
        title: "Better Uptime (Better Stack) Alternative",
        eyebrow: "comparing platforms",
        h1: "The Better Uptime (Better Stack) alternative you self-host",
        meta_description: "Better Uptime is now Better Stack. Want self-hosted monitoring and status pages you drive as code? Uptimepage is one AGPL binary with Terraform and MCP.",
        lede: "Better Uptime rebranded to Better Stack, and if it got too expensive or you want your data on your own servers, Uptimepage is a focused monitor and status page you run yourself. One binary, open source under AGPL, and everything you can click you can also declare in code. Start free on the hosted tier, no card.",
        features: &[
            Feature {
                label: "Run it",
                value: "hosted free, or self-host AGPL",
            },
            Feature {
                label: "Deploy",
                value: "one binary + docker compose up",
            },
            Feature {
                label: "As code",
                value: "Terraform provider + MCP",
            },
            Feature {
                label: "Probes",
                value: "multi-region, run your own",
            },
            Feature {
                label: "Checks",
                value: "HTTP, TCP, DNS, TLS, ping",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "yours to run",
                body: "The whole thing ships as one self-contained binary. `docker compose up` brings up the monitor with Postgres and ClickHouse, migrations run on boot, and the source is AGPL if you’d rather host it on your own servers.",
            },
            Section {
                heading: "no clicking through a UI",
                body: "Declare monitors, status pages and notification channels in HCL with the official Terraform provider, and point an LLM client at the MCP server to read your monitoring, with every write waiting on your approval.",
            },
            Section {
                heading: "checks from your own regions",
                body: "Run region agents on your own machines, wherever your customers actually are; each one authenticates with a scoped, org-bound token.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Monitoring as code",
                href: "/automation",
            },
            ResourceLink {
                label: "vs OneUptime",
                href: "/vs/oneuptime",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/oneuptime",
        created: "2026-06-19",
        lastmod: "2026-07-02",
        title: "A OneUptime Alternative That’s Quick to Run",
        eyebrow: "comparing open source",
        h1: "A OneUptime alternative that’s quick to run",
        meta_description: "An open-source monitor and status page that’s quick to run: one binary plus Postgres and ClickHouse, Terraform and MCP, AGPL. Free on the hosted tier.",
        lede: "Uptimepage is open source and focused on two jobs done well: uptime monitoring and a public status page. One binary plus two databases, up with a single command, or skip hosting it and use the free tier. No card.",
        features: &[
            Feature {
                label: "License",
                value: "AGPL, self-host",
            },
            Feature {
                label: "Stack",
                value: "one binary + Postgres + ClickHouse",
            },
            Feature {
                label: "Deploy",
                value: "docker compose up",
            },
            Feature {
                label: "As code",
                value: "Terraform provider + MCP",
            },
            Feature {
                label: "Status pages",
                value: "built in, branded",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "up in minutes",
                body: "One self-contained binary, Postgres for config and ClickHouse for the time-series. `docker compose up` and the whole stack is running with migrations applied. Nothing else to set up first.",
            },
            Section {
                heading: "drive it from a repo",
                body: "An official Terraform provider for monitors, status pages and channels, plus an MCP server so an LLM client can read your monitoring, with writes gated behind your approval and audited. Review your monitoring in a pull request.",
            },
            Section {
                heading: "hosted or self-hosted, you choose",
                body: "Start on the free hosted tier with no card, or run the AGPL source yourself. Switching later is an endpoint change, not a migration.",
            },
        ],
        code: None,
        resources: &[ResourceLink {
            label: "Monitoring as code",
            href: "/automation",
        }],
        cta: "Start free",
    },
    Landing {
        path: "/vs/uptime-kuma",
        created: "2026-06-20",
        lastmod: "2026-07-02",
        title: "An Uptime Kuma Alternative You Run as Code",
        eyebrow: "comparing open source",
        h1: "An Uptime Kuma alternative you run as code",
        meta_description: "Open-source uptime monitoring and branded status pages, managed as code with Terraform, a REST API and MCP. Team roles and subscribers. Free to start, no card.",
        lede: "Uptimepage is open source and does two jobs well: uptime monitoring and a public status page. Manage all of it as code, give your team roles, and let customers subscribe to status updates. Run the single binary yourself or use the free hosted tier. No card.",
        features: &[
            Feature {
                label: "License",
                value: "AGPL, self-host",
            },
            Feature {
                label: "Manage as code",
                value: "Terraform + REST API + MCP",
            },
            Feature {
                label: "Teams",
                value: "organizations, roles, invites",
            },
            Feature {
                label: "Status pages",
                value: "branded, with subscribers",
            },
            Feature {
                label: "Stack",
                value: "one binary + Postgres + ClickHouse",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "everything as code",
                body: "An official Terraform provider and a full REST API cover monitors, status pages and alert channels, and an MCP server lets an LLM client read your monitoring and act only with your approval, every write audited. Declare your monitoring in a repo and review changes in a pull request.",
            },
            Section {
                heading: "status pages your customers subscribe to",
                body: "Branded public pages on your own domain, with automatic incident detection, operator narration and maintenance windows. Visitors opt in with confirmed email or webhook and get notified on every change, with signed payloads they can verify.",
            },
            Section {
                heading: "built for teams",
                body: "Organizations with roles and invitations, isolated per tenant end to end. Run one instance for the whole team, or for every client, without sharing a single login.",
            },
            Section {
                heading: "probes you own",
                body: "Run regional probe agents on your own servers, wherever your users are, and Uptimepage folds their results into each monitor's health per region. Point the provider at the hosted tier or your own server; the config stays the same.",
            },
        ],
        code: Some(CodeSample {
            caption: "Declare a monitor in Terraform",
            body: r#"resource "uptimepage_target" "api" {
  name     = "api prod"
  interval = 60

  check = {
    type = "http"
    http = {
      url = "https://example.com/healthz"
      expected_status = {
        kind  = "exact"
        exact = 200
      }
    }
  }
}"#,
        }),
        resources: &[
            ResourceLink {
                label: "Monitoring as code",
                href: "/automation",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
            ResourceLink {
                label: "vs self-hosted monitors",
                href: "/vs/self-hosted-monitoring",
            },
            ResourceLink {
                label: "OpenStatus vs Uptime Kuma",
                href: "/compare/openstatus-vs-uptime-kuma",
            },
            ResourceLink {
                label: "Uptime Kuma vs Gatus",
                href: "/compare/uptime-kuma-vs-gatus",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/pingdom",
        created: "2026-06-25",
        lastmod: "2026-07-02",
        title: "Pingdom Alternative with Status Pages Built In",
        eyebrow: "switching monitors",
        h1: "A Pingdom alternative with status pages built in",
        meta_description: "Uptimepage pairs 60s HTTP, TCP, DNS and TLS checks with branded status pages and Slack, email and webhook alerts. Open source, free to start.",
        lede: "If you are comparing monitor prices, here is what Uptimepage gives you by default: the checks and a public status page are the same product, the source is open, and you can start free with no card.",
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
                value: "HTTP, TCP, DNS, TLS, ping",
            },
            Feature {
                label: "Alerts",
                value: "Slack, Telegram, PagerDuty, SMS + more",
            },
            Feature {
                label: "Run it",
                value: "hosted free, or self-host AGPL",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "monitoring and status page in one",
                body: "Checks and a public status page are the same product here, not a paid add-on. Flip any monitor public and it lands on your own subdomain with a 90-day history and per-component status.",
            },
            Section {
                heading: "timings that show the cause",
                body: "HTTP, TCP, DNS, TLS and ping, every minute from multiple regions. Every HTTP check’s timing is split across DNS, connect, TLS and time-to-first-byte, so a slow check tells you why.",
            },
            Section {
                heading: "own it, hosted or self-hosted",
                body: "Run it on the free hosted tier, or self-host the AGPL build as one binary with docker compose. Either way you drive it from the dashboard or as code with the Terraform provider and MCP.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Free pricing",
                href: "/pricing",
            },
            ResourceLink {
                label: "vs UptimeRobot",
                href: "/vs/uptimerobot",
            },
            ResourceLink {
                label: "Self-hosted status page",
                href: "/self-hosted-status-page",
            },
            ResourceLink {
                label: "vs Better Stack",
                href: "/vs/better-stack",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/self-hosted-status-pages",
        created: "2026-07-01",
        lastmod: "2026-07-02",
        title: "Uptimepage vs Upptime, Cachet & Statping",
        eyebrow: "comparing self-hosted",
        h1: "Uptimepage vs Upptime, Cachet and Statping",
        meta_description: "How Uptimepage compares to Upptime, Cachet and Statping in 2026: built-in monitoring, 60-second checks, status pages, subscribers and config-as-code.",
        lede: "Three popular self-hosted status tools, one honest table. Upptime and Statping run their own checks; Cachet is a status page that has only recently, and partially, added checks of its own. Here is where each fits, and where Uptimepage does both jobs in one product. Start free on the hosted tier or self-host under AGPL, no card.",
        features: &[
            Feature {
                label: "Built-in checks",
                value: "HTTP, TCP, DNS, TLS, ping",
            },
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Status page",
                value: "branded, subscribers",
            },
            Feature {
                label: "As code",
                value: "Terraform + REST + MCP",
            },
            Feature {
                label: "Run it",
                value: "hosted free, or self-host AGPL",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "Upptime: monitoring inside your GitHub repo",
                body: "Upptime is a neat idea. It runs checks as scheduled GitHub Actions, records history as commits in your repo, files incidents as GitHub Issues, and serves a static page from GitHub Pages. That design is also its limit. Actions cron will not run more than once every five minutes and can slip later under load, so detection is measured in minutes. There are no visitor subscriptions, checks run from a single region unless you add the third-party Globalping service, and there is no DNS-record or TLS-expiry check. Uptimepage runs its own checks every 60 seconds across HTTP, TCP, DNS, TLS and ping from several regions, and lets visitors subscribe by email or webhook.",
            },
            Section {
                heading: "Cachet: a status page catching up on monitoring",
                body: "Cachet began as a pure communication tool: you set components up or down by hand or over its API. Its actively developed v3, in the cachethq/core repo, is moving fast and, as of mid-2026, added basic HTTP component checks and subscriber management. The checks are real but young: HTTP GET only, no TCP, DNS or TLS, and you schedule the check command yourself rather than getting a built-in interval. It is still 3.x-dev with no stable release, incident email to subscribers is not wired up yet, it is a PHP and Laravel app with a database, queue and cron to operate, and it ships under a custom source-available license rather than an OSI open-source one. Uptimepage runs HTTP, TCP, DNS, TLS and ping checks every 60 seconds from multiple regions by default, opens incidents automatically, and is one binary to run.",
            },
            Section {
                heading: "Statping: close in shape, but barely maintained",
                body: "Statping is the nearest match here. It is a single Go binary that runs its own HTTP, TCP, UDP, ICMP and gRPC checks, draws response-time graphs, and shows incidents and maintenance on a themeable page. The problem is upkeep. The original project stopped in 2020, and the community statping-ng fork carries it now at roughly one release a year, the most recent in mid-2025. It has no visitor subscriptions, no multi-region checks, and no Terraform provider. Uptimepage does the same and adds config-as-code with Terraform, REST and MCP, team roles, subscriber pages and regional probes, hosted for free or self-hosted under AGPL.",
            },
            Section {
                heading: "One product, hosted or self-hosted",
                body: "The pattern is simple. Upptime and Statping monitor but leave out subscribers and multi-region; Cachet publishes but does not monitor. Uptimepage does both in one binary. Run docker compose up with Postgres and ClickHouse on your own servers, or start free on the hosted tier with no card. The REST API and Terraform provider work the same against both, so you can change your mind later.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Open-source status page",
                href: "/open-source-status-page",
            },
            ResourceLink {
                label: "Self-hosted status page",
                href: "/self-hosted-status-page",
            },
            ResourceLink {
                label: "vs Uptime Kuma",
                href: "/vs/uptime-kuma",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/self-hosted-monitoring",
        created: "2026-07-01",
        lastmod: "2026-07-02",
        title: "Uptime Kuma vs OpenStatus vs OneUptime vs Gatus",
        eyebrow: "comparing self-hosted",
        h1: "Uptimepage vs the self-hosted monitoring tools",
        meta_description: "How Uptimepage compares to Uptime Kuma, OpenStatus, OneUptime, Gatus and Kener in 2026: checks, status pages, multi-region probes and config-as-code.",
        lede: "The modern self-hosted crowd, compared honestly. Uptime Kuma and Gatus check the most protocols and run the lightest; OpenStatus and OneUptime match Uptimepage on config-as-code and multi-region; Kener has the prettiest status page. Uptimepage sits where monitoring, a real subscriber status page and Terraform, REST and MCP meet in one binary. Start free on the hosted tier or self-host under AGPL, no card.",
        features: &[
            Feature {
                label: "Built-in checks",
                value: "HTTP, TCP, DNS, TLS, ping, domain",
            },
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Status page",
                value: "branded, subscribers",
            },
            Feature {
                label: "Probes",
                value: "multi-region, run your own",
            },
            Feature {
                label: "As code",
                value: "Terraform + REST + MCP",
            },
            Feature {
                label: "Run it",
                value: "one binary, hosted or AGPL",
            },
        ],
        sections: &[
            Section {
                heading: "Uptime Kuma: the broadest checks, the lightest footprint",
                body: "Uptime Kuma is the community favourite for good reason: around forty monitor types (databases, gRPC, MQTT, SNMP, Steam, real-browser, push heartbeats), roughly ninety-five alert integrations, 20-second intervals, and a single container to run. Its weak side is teams and status pages. It is single-user with no roles, it is driven entirely over a socket API with no REST or Terraform, its status pages take an RSS feed rather than email or webhook subscribers, and incidents are posted by hand, not opened from a failing check. Uptimepage trades some of that protocol breadth for a subscriber status page, organizations with roles, auto-opened incidents and config-as-code.",
            },
            Section {
                heading: "OpenStatus and OneUptime: the dev-first platforms",
                body: "These are the closest to Uptimepage in philosophy. OpenStatus is monitoring-as-code done well: a Terraform provider, a CLI, an MCP server, auto-resolving incidents, email and webhook subscribers, and probes across twenty-eight regions with sub-minute checks. Its trade-offs are a heavier stack (Turso plus Tinybird plus hosted queues) and an open-source checker that implements only HTTP, TCP and DNS, with ICMP, UDP and SSL-certificate monitors declared in config but not built. OneUptime does everything Uptimepage does and then adds on-call scheduling, escalation, logs, tracing and APM, but that reach costs you a Postgres, ClickHouse, Redis and many-service deployment to operate. Uptimepage aims at the same developer surface, Terraform, REST and MCP, but as one binary you can actually run. It matches those sub-minute checks too: 30 seconds on Pro and 10 seconds self-hosted, while the free founding plan already carries fifty monitors at sixty seconds.",
            },
            Section {
                heading: "Gatus: the protocol-rich checker",
                body: "Gatus is a joy if you want declarative checks in version control. Eleven endpoint protocols including gRPC, SSH, WebSocket, STARTTLS and UDP, a rich condition language with JSONPath body assertions and certificate-expiry checks, multi-step suites, and a tiny static binary with an optional zero-database mode. What it is not is a status page. It ships a health dashboard with badges, not a branded page with subscribers, it has no incident timeline, and it is single-tenant behind one basic-auth or OIDC boundary. Uptimepage covers the everyday HTTP, TCP, DNS, TLS and ping checks and pairs them with the public status page, subscribers and multi-tenant teams Gatus leaves out.",
            },
            Section {
                heading: "Kener: the polished status page",
                body: "Kener is the best-looking status page of the group: separate light and dark palettes, custom CSS and footer HTML, twenty-four locales, embeddable widgets, four badge styles and custom RBAC roles. It checks real services too, including gRPC, SQL and heartbeats. The gaps are on the monitoring platform side: no multi-region probing, four alert channels (email, webhook, Slack, Discord), email-and-RSS subscribers only, a single tenant, and a hard Redis dependency. Uptimepage gives up a little status-page theming for multi-region probes, more alert channels and config-as-code.",
            },
            Section {
                heading: "Where Uptimepage fits",
                body: "Uptimepage is not the very fastest interval or the widest protocol list here, and it is honest about that. What it does is put the two halves together: real HTTP, TCP, DNS, TLS-certificate, ping and domain-expiry monitoring, and a branded public status page with confirmed email and webhook subscribers, auto-opened incidents and scheduled maintenance. All of it is driven from code with a Terraform provider, a full REST API and an MCP server, isolated per organization with roles, and checked from probes you can run in any region. It runs as one binary with Postgres and ClickHouse, hosted for free or self-hosted under AGPL.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "vs Upptime, Cachet, Statping",
                href: "/vs/self-hosted-status-pages",
            },
            ResourceLink {
                label: "vs Uptime Kuma",
                href: "/vs/uptime-kuma",
            },
            ResourceLink {
                label: "vs OneUptime",
                href: "/vs/oneuptime",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
            ResourceLink {
                label: "OpenStatus vs Uptime Kuma",
                href: "/compare/openstatus-vs-uptime-kuma",
            },
            ResourceLink {
                label: "Uptime Kuma vs Gatus",
                href: "/compare/uptime-kuma-vs-gatus",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/compare/openstatus-vs-uptime-kuma",
        created: "2026-07-05",
        lastmod: "2026-07-05",
        title: "OpenStatus vs Uptime Kuma",
        eyebrow: "comparing self-hosted",
        h1: "OpenStatus vs Uptime Kuma: which fits how you work?",
        meta_description: "OpenStatus and Uptime Kuma compared on facts: monitoring as code with hosted probes vs the click-driven self-hosted classic, and where each stops. July 2026.",
        lede: "One is monitoring as code with a hosted multi-region fleet, the other is the most-starred self-hosted dashboard on GitHub. Both are open source and both are good; they assume very different teams. The facts first, then where Uptimepage sits between them.",
        features: &[],
        sections: &[
            Section {
                heading: "Two philosophies, not two feature lists",
                body: "The real difference is who drives. Uptime Kuma is UI-first: you click monitors into a dashboard, and the configuration lives in its database. There is no official REST API for managing monitors and no Terraform provider, which is fine for one person and painful for a team with review habits. OpenStatus starts from the other end: monitors are YAML, CLI commands, GitHub Actions or Terraform, and the dashboard is one view of that config, with a full REST API underneath.",
            },
            Section {
                heading: "Where Uptime Kuma is ahead",
                body: "Breadth and community. Kuma speaks around forty monitor types by default, including databases, MQTT, SNMP and a real Chromium browser check, and it can notify roughly ninety-five services. It installs in one container in five minutes, checks as often as every 20 seconds, and has by far the largest community of any tool in this space, which means answers exist for almost any problem you hit.",
            },
            Section {
                heading: "Where OpenStatus is ahead",
                body: "Teams and vantage points. OpenStatus runs a hosted probe fleet across twenty-eight regions on three cloud providers, so you see your service the way users on other continents do, without running agents yourself. It has organizations with unlimited members on paid tiers, email and RSS subscribers on its status pages, and auto-resolving incident handling. Kuma is single-login with no roles, checks from wherever you installed it, and its status pages have no subscriber notifications.",
            },
            Section {
                heading: "The honest caveats on both",
                body: "OpenStatus self-hosted is a multi-service TypeScript stack with external database dependencies, harder to operate than Kuma's single container, and its open-source checker covers fewer protocols than its API schema advertises. Kuma's limits are structural: multi-user support and a management API have been open feature requests for years because the architecture was built for one operator with a browser.",
            },
            Section {
                heading: "Where Uptimepage fits",
                body: "Uptimepage sits deliberately between them: one Rust binary built for teams the way Kuma isn't, with the as-code approach OpenStatus is known for. You get HTTP, TCP, DNS, TLS and ping checks, organizations with roles, a Terraform provider, a REST API and an MCP server, plus a branded status page with confirmed email and webhook subscribers and auto-opened incidents. Probes are multi-region and you can run your own. Hosted free with no card, or self-host under AGPL with docker compose.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Uptimepage vs Uptime Kuma",
                href: "/vs/uptime-kuma",
            },
            ResourceLink {
                label: "The self-hosted field, compared",
                href: "/vs/self-hosted-monitoring",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/compare/uptime-kuma-vs-gatus",
        created: "2026-07-05",
        lastmod: "2026-07-05",
        title: "Uptime Kuma vs Gatus",
        eyebrow: "comparing self-hosted",
        h1: "Uptime Kuma vs Gatus: clicks or YAML?",
        meta_description: "Uptime Kuma configures in a UI, Gatus lives in YAML. Check types, status pages, alerting and team features compared honestly. July 2026.",
        lede: "The two most-loved self-hosted monitors answer one question differently: should monitoring be clicked together in a dashboard, or declared in a file and reviewed in a pull request? Everything else follows from that split.",
        features: &[],
        sections: &[
            Section {
                heading: "The split that decides it",
                body: "Uptime Kuma is a dashboard you click: add a monitor, pick a type, wire a notification, all stored in its database. Gatus has no editing UI: every endpoint is YAML in version control, the web UI is read-only, and a change means a config redeploy. Neither is wrong. One fits a homelab and a person who thinks in browsers; the other fits an engineer who thinks in Git and wants monitoring reviewed like code.",
            },
            Section {
                heading: "What each does well",
                body: "Kuma wins on reach: around forty monitor types including databases, MQTT, SNMP and a real browser check, roughly ninety-five notification services, 20-second intervals, and the biggest community in the category. Gatus wins on discipline: eleven endpoint protocols including gRPC, SSH, WebSocket and UDP, a condition language that asserts on status, response time, JSON body paths and certificate expiry, multi-step suites, and a tiny static Go binary that can even run without a database.",
            },
            Section {
                heading: "What neither gives you",
                body: "A customer-facing status page with subscribers, and a team. Kuma's status pages are real but nobody can subscribe to them, and the whole app is one shared login. Gatus's dashboard doubles as its status page: fine for an internal dashboard, not something you show customers, and its access control is one basic-auth or OIDC gate. Both check from a single vantage point unless you assemble more instances yourself.",
            },
            Section {
                heading: "Where Uptimepage fits",
                body: "If the YAML-versus-clicks debate ends with 'actually we need customers to see a status page and teammates to have accounts', that is the gap Uptimepage fills. Checks over HTTP, TCP, DNS, TLS and ping, configured in the UI or declared with the Terraform provider and REST API, organizations with roles, multi-region probes you can run yourself, and a branded status page with email and webhook subscribers where incidents open automatically. One binary, hosted free or AGPL self-hosted.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Uptimepage vs Uptime Kuma",
                href: "/vs/uptime-kuma",
            },
            ResourceLink {
                label: "The self-hosted field, compared",
                href: "/vs/self-hosted-monitoring",
            },
            ResourceLink {
                label: "OpenStatus vs Uptime Kuma",
                href: "/compare/openstatus-vs-uptime-kuma",
            },
            ResourceLink {
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/compare/pingdom-vs-statuscake",
        created: "2026-07-05",
        lastmod: "2026-07-05",
        title: "Pingdom vs StatusCake",
        eyebrow: "comparing hosted monitors",
        h1: "Pingdom vs StatusCake: what you actually get",
        meta_description: "Pingdom and StatusCake compared on facts: pricing models, check types, intervals, probe locations and the status page catch. July 2026.",
        lede: "Two of the oldest names in hosted uptime monitoring, built for different buyers. Pingdom is a digital-experience suite inside the SolarWinds portfolio; StatusCake is an independent UK product with a generous range of plans. The facts first, then where Uptimepage sits.",
        features: &[],
        sections: &[
            Section {
                heading: "The pricing split",
                body: "StatusCake has a real free tier: ten uptime monitors at five-minute intervals, plus single allowances of its page speed, domain and SSL products. Pingdom has no free tier at all, only a 30-day trial, and then usage-based pricing where uptime checks, transaction checks and RUM pageviews are each priced on their own scale. Both geo-localize prices, so we describe the pricing model rather than exact numbers; check their pricing pages for your currency.",
            },
            Section {
                heading: "What each does well",
                body: "Pingdom is the fuller experience suite: scripted browser transactions, real user monitoring with 13-month retention, roughly a hundred probe locations, and unlimited users on every plan. StatusCake covers more protocols for the money: HTTP, HEAD, TCP, DNS, SMTP, SSH, ping and push heartbeats, with SSL, domain-expiry and basic Linux server monitoring bundled into the same plans, and one-minute checks arriving on its first paid tier.",
            },
            Section {
                heading: "The status page problem",
                body: "Read this before picking either for a customer-facing status page. Pingdom includes public status pages in its plans. StatusCake sells status pages as a separate product with its own tiers, capped by page count and subscriber count, billed on top of monitoring. If the status page is the point, that add-on can cost more than the monitoring beside it.",
            },
            Section {
                heading: "Where Uptimepage fits",
                body: "Uptimepage does not do browser transactions or RUM, and says so plainly. What it does is pair the monitoring with the status page in one product and one price: HTTP, TCP, DNS, TLS, ping and domain checks every 60 seconds on the free tier, a branded status page with confirmed email and webhook subscribers included, incidents that open automatically, and a Terraform provider, REST API and MCP server for teams who keep config in code. It is also open source under AGPL, so you can always self-host instead of being locked in.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Uptimepage vs Pingdom",
                href: "/vs/pingdom",
            },
            ResourceLink {
                label: "Uptimepage vs UptimeRobot",
                href: "/vs/uptimerobot",
            },
            ResourceLink {
                label: "The self-hosted field, compared",
                href: "/vs/self-hosted-monitoring",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/automation",
        created: "2026-06-16",
        lastmod: "2026-06-21",
        title: "Monitoring as Code: Terraform & MCP",
        eyebrow: "for developers & devops",
        h1: "Run your monitoring from code, not clicks",
        meta_description: "Manage Uptimepage monitors, status pages and alerts as code with the Terraform provider, and connect any LLM over MCP. Free to start, no card.",
        lede: "Everything you can click in Uptimepage you can declare in code. Provision monitors and status pages with the Terraform provider, and let an AI assistant read your monitoring over MCP, with the same tenant isolation, scopes and rate limits as the dashboard.",
        features: &[
            Feature {
                label: "Terraform provider",
                value: "uptimepage/uptimepage",
            },
            Feature {
                label: "Terraform resources",
                value: "monitors, status pages, channels",
            },
            Feature {
                label: "MCP endpoint",
                value: "mcp.uptimepage.dev/mcp",
            },
            Feature {
                label: "MCP access",
                value: "OAuth one-click, read + fenced writes",
            },
            Feature {
                label: "API tokens",
                value: "scoped, least-privilege, expiring",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "config-as-code with Terraform",
                body: "Declare monitors, status pages, components and notification channels in HCL with the official provider (source uptimepage/uptimepage). Review changes in a pull request, roll them out across orgs, and keep your monitoring reproducible instead of hand-clicked.",
            },
            Section {
                heading: "ask an assistant what’s broken",
                body: "The MCP server lets an LLM client (Claude, an IDE, anything that speaks MCP) read your monitors and incidents and take tightly-fenced actions. It runs inside the same app, so the scope checks and rate limits that guard your data guard the assistant’s access too.",
            },
            Section {
                heading: "tokens that do one job",
                body: "Automation authenticates with scoped API tokens: resource-and-action permissions, bound to one org, with an enforced expiry. Mint a read-only token for a dashboard or a write-scoped one for Terraform, never an all-or-nothing key.",
            },
        ],
        code: Some(CodeSample {
            caption: "Declare a monitor in Terraform",
            body: r#"terraform {
  required_providers {
    uptimepage = {
      source = "uptimepage/uptimepage"
    }
  }
}

resource "uptimepage_target" "api" {
  name     = "api prod"
  interval = 60

  check = {
    type = "http"
    http = {
      url = "https://example.com/healthz"
      expected_status = {
        kind  = "exact"
        exact = 200
      }
    }
  }
}"#,
        }),
        resources: &[
            ResourceLink {
                label: "Terraform Registry",
                href: TERRAFORM_URL,
            },
            ResourceLink {
                label: "Terraform uptime monitoring",
                href: "/terraform-uptime-monitoring",
            },
            ResourceLink {
                label: "MCP server",
                href: "/mcp-server",
            },
            ResourceLink {
                label: "How the MCP server works",
                href: "/blog/mcp-server",
            },
            ResourceLink {
                label: "For developers",
                href: "/uptime-monitoring-for-developers",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/terraform-uptime-monitoring",
        created: "2026-06-25",
        lastmod: "2026-06-25",
        title: "Terraform Uptime Monitoring with Status Pages",
        eyebrow: "infrastructure as code",
        h1: "Uptime monitoring you declare in Terraform",
        meta_description: "Declare uptime monitors, status pages and alerts in Terraform with the official Uptimepage provider. HTTP, TCP, DNS, TLS checks. Free to start.",
        lede: "Provision a monitor the same way you provision the service it watches. The official Uptimepage provider manages monitors, status pages, components and notification channels in HCL, so every new service ships with monitoring instead of a follow-up ticket.",
        features: &[
            Feature {
                label: "Terraform provider",
                value: "uptimepage/uptimepage",
            },
            Feature {
                label: "Resources",
                value: "monitors, pages, channels",
            },
            Feature {
                label: "Check types",
                value: "HTTP, TCP, DNS, TLS",
            },
            Feature {
                label: "Check interval",
                value: "every 60s",
            },
            Feature {
                label: "Auth",
                value: "scoped, expiring API tokens",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "monitoring ships with the service",
                body: "Declare the monitor next to the resource it watches. Every service gets consistent monitoring from its first apply, with no gap between deploy and the first check.",
            },
            Section {
                heading: "review it like any other change",
                body: "Monitors, status pages and alert channels live in HCL, so a change is a pull request with a plan and an apply. Roll the same config across orgs and keep your monitoring reproducible instead of hand-clicked.",
            },
            Section {
                heading: "more than Terraform",
                body: "The same data model answers a full REST API and an MCP server, so an assistant can read your monitors while Terraform owns their shape. Everything you can click, you can declare.",
            },
        ],
        code: Some(CodeSample {
            caption: "Declare a monitor in Terraform",
            body: r#"terraform {
  required_providers {
    uptimepage = {
      source = "uptimepage/uptimepage"
    }
  }
}

resource "uptimepage_target" "api" {
  name     = "api prod"
  interval = 60

  check = {
    type = "http"
    http = {
      url = "https://example.com/healthz"
      expected_status = {
        kind  = "exact"
        exact = 200
      }
    }
  }
}"#,
        }),
        resources: &[
            ResourceLink {
                label: "Terraform Registry",
                href: TERRAFORM_URL,
            },
            ResourceLink {
                label: "Monitoring as code",
                href: "/automation",
            },
            ResourceLink {
                label: "MCP server",
                href: "/mcp-server",
            },
            ResourceLink {
                label: "For developers",
                href: "/uptime-monitoring-for-developers",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/mcp-server",
        created: "2026-06-18",
        lastmod: "2026-06-21",
        title: "MCP Server for Uptime Monitoring",
        eyebrow: "for ai & llm workflows",
        h1: "Ask an AI what’s broken, over MCP",
        meta_description: "Connect any LLM to your uptime monitoring over MCP. Read monitors and incidents, take fenced actions, one-click OAuth. Free to start, no card.",
        lede: "Point a Model Context Protocol client (Claude, an IDE, anything that speaks MCP) at your monitoring and ask it what’s down in plain language. Read tools answer from your real monitors; write tools take action only behind your explicit approval. Same tenant isolation, scopes and rate limits as the dashboard.",
        features: &[
            Feature {
                label: "MCP endpoint",
                value: "mcp.uptimepage.dev/mcp",
            },
            Feature {
                label: "Connect",
                value: "OAuth one-click, or scoped token",
            },
            Feature {
                label: "Tools",
                value: "13 (read + fenced writes)",
            },
            Feature {
                label: "Every write",
                value: "your approval + an audit row",
            },
            Feature {
                label: "Clients",
                value: "Claude, IDEs, any MCP client",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "ask your monitoring in plain language",
                body: "Read tools hand the model the same forensics a good engineer reaches for: which monitor is down and since when, an incident’s full timeline, and why a check is slow: DNS, connect, TLS handshake and time-to-first-byte reported separately, so \"slow because TLS\" and \"slow because DNS\" come back as different answers.",
            },
            Section {
                heading: "actions stay behind a human",
                body: "Most tools can only look. The few that act (run a check, pause or resume a monitor, post to an incident) can’t fire without a scoped token, your in-the-moment approval naming the exact effect, and an audit row for every outcome. There is no \"remember my choice.\"",
            },
            Section {
                heading: "your data can’t hijack the assistant",
                body: "A monitor name or scraped error text is written by someone else, and now an LLM is reading it. Every piece of customer-supplied text reaches the model labelled as data to report, never instructions to act on. Even a fooled model still can’t act without your out-of-band approval.",
            },
            Section {
                heading: "one-click OAuth, no copy-paste",
                body: "Your client discovers the server, you log in with the session you already have, approve a consent screen, and a scoped, org-bound, expiring token is minted behind the scenes. The one lifetime the consent screen won’t offer is \"never expires.\"",
            },
        ],
        code: Some(CodeSample {
            caption: "Point an MCP client at the server",
            body: r#"{
  "mcpServers": {
    "uptimepage": {
      "url": "https://mcp.uptimepage.dev/mcp"
    }
  }
}"#,
        }),
        resources: &[
            ResourceLink {
                label: "How the MCP server works",
                href: "/blog/mcp-server",
            },
            ResourceLink {
                label: "For developers",
                href: "/uptime-monitoring-for-developers",
            },
            ResourceLink {
                label: "Monitoring as code",
                href: "/automation",
            },
        ],
        cta: "Start free",
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
    code: Option<&'static CodeSample>,
    matrix: Option<&'static Matrix>,
    resources: &'static [ResourceLink],
    cta: &'static str,
    canonical_url: String,
    og: OpenGraph,
    breadcrumb_json_ld: JsonLd,
    /// Absent on `/compare/` pages: their main entity is the rival pair,
    /// and app markup there would misdescribe the page.
    software_json_ld: Option<JsonLd>,
    webpage_json_ld: JsonLd,
    faq_json_ld: Option<JsonLd>,
    faqs: &'static [(&'static str, &'static str)],
    app_url: String,
    version: &'static str,
}

/// Per-page FAQ for the landings that have one; others render no FAQ. Most
/// comparison answers describe Uptimepage only, matching the neutral-comparison
/// rule above; the head-to-head page's answers also state verifiable, dated
/// competitor facts, in step with its matrix.
fn page_faqs(path: &str) -> &'static [(&'static str, &'static str)] {
    match path {
        "/compare/openstatus-vs-uptime-kuma" => &[
            (
                "Is OpenStatus or Uptime Kuma easier to self-host?",
                "Uptime Kuma, clearly. It is one Docker container. OpenStatus self-hosted is a multi-service TypeScript stack with external database dependencies; its hosted tier exists precisely because running it is work.",
            ),
            (
                "Does Uptime Kuma have an API or Terraform provider?",
                "No official REST API for managing monitors and no Terraform provider. Its API keys only expose metrics. OpenStatus and Uptimepage both offer Terraform, a REST API and CLI-style workflows.",
            ),
            (
                "Which one can my customers subscribe to?",
                "OpenStatus status pages take email and RSS subscribers. Uptime Kuma pages have no subscriber notifications. Uptimepage pages take confirmed email and webhook subscribers, and incidents open automatically from failing checks.",
            ),
            (
                "Is Uptime Kuma still fine for a homelab?",
                "Yes, and it is probably the best pick there. The comparison only gets interesting once a second person needs access, customers need a status page, or you want config in version control.",
            ),
        ],
        "/compare/pingdom-vs-statuscake" => &[
            (
                "Does Pingdom have a free plan?",
                "No. Pingdom offers a 30-day trial, then paid usage-based plans. StatusCake keeps a permanent free tier with ten monitors at five-minute intervals, and Uptimepage's free tier checks every 60 seconds with no card.",
            ),
            (
                "Are StatusCake status pages included in its plans?",
                "No. StatusCake Pages is a separately billed product with its own tiers, capped by pages and subscribers. Pingdom includes status pages in its plans, and Uptimepage includes a branded page with subscribers on every tier.",
            ),
            (
                "Which one does synthetic browser monitoring?",
                "Pingdom. Scripted browser transactions and real user monitoring are its core products. StatusCake offers page speed checks but no RUM. Uptimepage does neither; it focuses on uptime checks and status pages.",
            ),
            (
                "Can I self-host either of them?",
                "No, both are closed SaaS. If owning the stack matters, that is a different category; Uptimepage is AGPL open source, so the hosted tier has a self-hosted exit.",
            ),
        ],
        "/compare/uptime-kuma-vs-gatus" => &[
            (
                "Should I pick Uptime Kuma or Gatus?",
                "Pick by workflow. If you want to click monitors together in a dashboard, Kuma. If you want every check declared in YAML, reviewed in a pull request and deployed like code, Gatus. Feature lists matter less than that split.",
            ),
            (
                "Can Gatus replace a status page?",
                "For an internal ops wall, yes: its dashboard shows health, badges and announcements. For customers, no: there are no subscribers, no incident timeline and no branding beyond what you build around it.",
            ),
            (
                "Which is lighter to run?",
                "Gatus. It is a small static Go binary that can even run without a database. Kuma is a Node.js app in one container, still light, just not that light.",
            ),
            (
                "What if I need teams or an API?",
                "Neither has real multi-user support or a management API. That is the gap tools like Uptimepage and OpenStatus fill: organizations with roles, a REST API and a Terraform provider on top of the checks.",
            ),
        ],
        "/open-source-status-page" => &[
            (
                "Is the status page really open source?",
                "Yes. Uptimepage is AGPL, so you can read the source, run it, and modify it. The hosted tier is $0 a month if you would rather not host it.",
            ),
            (
                "Does it monitor, or just publish?",
                "Both. Uptime monitoring is built in, so incidents open automatically from real HTTP, TCP, DNS, TLS and ping checks and appear on the page without a second tool.",
            ),
            (
                "Can customers subscribe to updates?",
                "Yes. Visitors opt in with confirmed email or webhook and hear about every incident and maintenance change, with signed payloads they can verify.",
            ),
            (
                "Can I self-host it?",
                "Yes. The AGPL source ships a compose file: one command brings up the binary with Postgres and ClickHouse, migrations applied on boot.",
            ),
        ],
        "/open-source-uptime-monitoring" => &[
            (
                "Is Uptimepage really open source?",
                "Yes. The source is AGPL, so you can read it, run it, and modify it. If you would rather not host it, the hosted tier is free with no card.",
            ),
            (
                "Can I self-host the uptime monitor?",
                "Yes. Clone the repo and run `docker compose up`. That brings up the single binary with Postgres and ClickHouse and applies migrations on boot. No Kubernetes to operate.",
            ),
            (
                "What can it monitor?",
                "HTTP, TCP, DNS, TLS-certificate and domain expiry, ICMP ping, cron-job heartbeats and scripted browser login flows, every 60 seconds from as many regions as you run.",
            ),
            (
                "Does it include a status page?",
                "Yes. Incidents open automatically from failing checks and flow onto a branded public status page your customers can subscribe to, all from the same binary.",
            ),
            (
                "Is it free?",
                "Self-hosting under AGPL is free. The hosted tier is also $0 a month if you prefer not to run it yourself.",
            ),
        ],
        "/self-hosted-status-page" => &[
            (
                "How do I deploy it?",
                "Clone the repo and run `docker compose up`. That starts the single binary with Postgres and ClickHouse, runs migrations on boot, and serves the dashboard and public status page.",
            ),
            (
                "Where does my data live?",
                "On your own infrastructure. Self-hosting keeps every check result, incident and subscriber in your environment, and the public page serves straight from it.",
            ),
            (
                "Can I monitor from more than one region?",
                "Yes. Run regional probe agents on your own servers and Uptimepage folds their results into each monitor per region.",
            ),
            (
                "Is it free?",
                "Yes. The source is AGPL and free to self-host, and the hosted tier is $0 a month if you prefer not to run it.",
            ),
        ],
        "/white-label-uptime-monitoring" => &[
            (
                "Can I put my own brand on the status page?",
                "Yes. Every page carries your logo and colours on your own subdomain. To drop the powered-by badge entirely, use the Pro plan or a self-hosted instance.",
            ),
            (
                "Can I manage many clients from one account?",
                "Yes. Add every client as a monitor, group them, and give each a branded page. One account covers the whole roster, with no per-client tool or invoice.",
            ),
            (
                "Is there per-client or per-seat pricing?",
                "No. The hosted tier is free with no card, and paid Pro is a flat plan. Self-hosting under AGPL is free as well.",
            ),
            (
                "Can I remove every trace of the vendor?",
                "Self-host the AGPL binary and no outside brand appears anywhere in your stack, or upgrade to Pro to drop the badge on the hosted tier. Your brand is the only one your clients see.",
            ),
        ],
        "/uptime-monitoring-for-developers" => &[
            (
                "Can I manage monitors as code?",
                "Yes. An official Terraform provider covers monitors, status pages and channels, so you declare them in HCL and review changes in a pull request.",
            ),
            (
                "Is there a REST API?",
                "Yes, a full REST API mirroring the dashboard, authenticated with scoped, org-bound tokens you can narrow to a single job.",
            ),
            (
                "Does it work with LLM tooling?",
                "Yes. An MCP server lets an LLM client read your monitoring and take approval-gated, audited actions from the same config that lives in your repo.",
            ),
            (
                "Can I self-host it?",
                "Yes. The whole product is one AGPL binary; compose brings it up next to Postgres and ClickHouse in minutes.",
            ),
        ],
        "/vs/uptimerobot" => &[
            (
                "Is Uptimepage free?",
                "Yes. The hosted tier is $0 a month with no credit card, and the AGPL source is free to self-host.",
            ),
            (
                "Does it include a public status page?",
                "It does: a branded status page on your own subdomain, with automatic incident detection, maintenance windows, and email or webhook subscribers.",
            ),
            (
                "Can I manage monitors as code?",
                "Yes. There is an official Terraform provider, a full REST API, and an MCP server, so you can declare monitors in a repo and review changes in a pull request.",
            ),
            (
                "Can I self-host it?",
                "Yes. Everything compiles to a single AGPL binary you run with compose, so the whole stack sits on hardware you control.",
            ),
        ],
        "/vs/statuspage" => &[
            (
                "Does Uptimepage monitor as well as publish?",
                "Yes. Uptime monitoring is built in, so incidents open automatically from real HTTP, TCP, DNS, TLS and ping checks and flow straight onto the status page.",
            ),
            (
                "Is a custom domain included?",
                "Every org gets a branded subdomain by default, and a custom CNAME is coming. Branding, logo and colours are included, not gated behind a higher tier.",
            ),
            (
                "Is it free?",
                "Yes: $0 a month, no credit card, and no per-page pricing. Self-hosting under AGPL is free as well.",
            ),
            (
                "Can customers subscribe to updates?",
                "They can. Visitors opt in with confirmed email or webhook and hear about every incident and maintenance change, with signed payloads they can verify.",
            ),
        ],
        "/vs/better-stack" => &[
            (
                "Is Better Stack the same as Better Uptime?",
                "Broadly, yes. Better Uptime was folded into Better Stack, where it is now the Uptime product, so this comparison applies whichever name you searched for.",
            ),
            (
                "Can I self-host Uptimepage?",
                "Yes. It ships as one AGPL binary with Postgres and ClickHouse, so `docker compose up` puts it live with your data on your own servers.",
            ),
            (
                "Is there per-seat or per-monitor pricing?",
                "No. No per-seat or per-monitor metering. The hosted tier is free with no credit card, and paid Pro is a flat plan, not metered.",
            ),
            (
                "Can I run it as code?",
                "Yes. An official Terraform provider, a REST API, and an MCP server mean everything you can click, you can also declare.",
            ),
            (
                "Does it do incident paging?",
                "Yes. It pages your team on Slack, Telegram, WhatsApp, SMS, PagerDuty and more, and the reminders repeat until someone acknowledges.",
            ),
        ],
        "/vs/oneuptime" => &[
            (
                "How heavy is Uptimepage to run?",
                "It is one self-contained binary plus Postgres and ClickHouse. `docker compose up` brings the whole stack up, with no Kubernetes to operate.",
            ),
            (
                "Is it open source?",
                "Yes, AGPL. Run it yourself for free, or start on the free hosted tier with no card.",
            ),
            (
                "Can I manage it as code?",
                "Yes. An official Terraform provider, a REST API, and an MCP server share the same data model hosted or self-hosted.",
            ),
            (
                "Does it include status pages and incidents?",
                "It does: branded public status pages, automatic incident detection, maintenance windows, and email or webhook subscribers.",
            ),
        ],
        "/vs/uptime-kuma" => &[
            (
                "Is Uptimepage a good Uptime Kuma alternative?",
                "It covers the same self-hosted monitoring ground and adds config-as-code, organizations with roles, and subscriber status pages, as one binary or a free hosted tier.",
            ),
            (
                "Can I manage monitors as code?",
                "Yes. An official Terraform provider, a full REST API, and an MCP server let you declare monitors in a repo and review changes in a pull request.",
            ),
            (
                "Does it support teams?",
                "Yes. Organizations come with roles and invitations and are isolated per tenant, so nobody shares a single login.",
            ),
            (
                "Is it free to self-host?",
                "Yes. The AGPL source runs with `docker compose up` on Postgres and ClickHouse, and the hosted tier is $0 a month.",
            ),
        ],
        "/vs/self-hosted-status-pages" => &[
            (
                "Does Cachet do monitoring?",
                "As of mid-2026, partly. Cachet v3 added basic HTTP checks you schedule yourself (GET only, no TCP, DNS or TLS), though it is still in development with no stable release. Uptimepage runs HTTP, TCP, DNS, TLS and ping checks every 60 seconds from multiple regions and opens incidents automatically.",
            ),
            (
                "How often can Upptime check?",
                "Upptime runs on GitHub Actions cron, which cannot fire more than once every five minutes and can drift later under load. Uptimepage checks as often as every 60 seconds from multiple regions.",
            ),
            (
                "Is Statping still maintained?",
                "The original Statping stopped in 2020. A community fork, statping-ng, keeps it going at roughly one release a year. Uptimepage is actively developed, with config-as-code, subscriber pages and regional probes, hosted or self-hosted.",
            ),
            (
                "Can I self-host Uptimepage?",
                "Yes, that is the point of it. One AGPL binary, Postgres, ClickHouse, one compose file. Or stay on the free hosted tier and let us run it.",
            ),
        ],
        "/vs/self-hosted-monitoring" => &[
            (
                "Which of these is the most lightweight to run?",
                "Gatus (a tiny static binary, optionally zero-database) and Uptime Kuma (one container) are the lightest. OneUptime is the heaviest, needing Postgres, ClickHouse, Redis and many services. Uptimepage sits in between: one binary with Postgres and ClickHouse.",
            ),
            (
                "Which support monitoring as code?",
                "Uptimepage, OpenStatus and OneUptime all offer a Terraform provider plus an MCP server. Gatus is declarative YAML by nature but has no Terraform provider, and Uptime Kuma is driven over a socket API with no REST or Terraform.",
            ),
            (
                "Do they all have status-page subscribers?",
                "Uptimepage, OpenStatus, OneUptime and Kener let visitors subscribe (email, and webhook or more). Uptime Kuma offers an RSS feed only, and Gatus is a health dashboard with no subscriber feature.",
            ),
            (
                "Can I self-host Uptimepage?",
                "Yes. Same one-binary deploy the table describes: compose up, migrations on boot, AGPL. The hosted tier exists for when you would rather not run it.",
            ),
        ],
        "/status-page-for-saas" => &[
            (
                "Can I put the status page on my own domain?",
                "Every org gets a branded status page on its own subdomain with your logo and colours, and a custom CNAME is on the way.",
            ),
            (
                "How fast does it detect an outage?",
                "Checks run as often as every 60 seconds from multiple regions, and a failing check opens an incident automatically and posts it to the page.",
            ),
            (
                "Will the status page stay up when my app is down?",
                "Yes. The public page is cached and served independently, so it keeps loading even when the service it reports on is struggling.",
            ),
            (
                "Can customers subscribe to updates?",
                "Yes. Visitors opt in with confirmed email or webhook and hear about every incident and maintenance change, with signed payloads they can verify.",
            ),
        ],
        "/status-page-for-agencies" => &[
            (
                "Can I manage many clients from one account?",
                "Yes. Watch every client site from a single dashboard and give each client its own branded status page.",
            ),
            (
                "Does each client get a separate branded page?",
                "Yes. Each status page carries that client’s own logo and colours on its own subdomain.",
            ),
            (
                "Can I control who sees what?",
                "Organizations come with roles and invitations and are isolated per tenant, so teammates and clients only see what you grant them.",
            ),
            (
                "Is it free to start?",
                "Yes. The hosted tier is $0 a month with no credit card, and the AGPL source is free to self-host.",
            ),
        ],
        "/automation" => &[
            (
                "How do I manage monitors as code?",
                "Use the official Terraform provider to declare monitors, status pages and notification channels in HCL, then review changes in a pull request.",
            ),
            (
                "Is there a REST API?",
                "Yes. A full REST API backs everything, with scoped, org-bound tokens you can set to expire.",
            ),
            (
                "What can the MCP server do?",
                "An LLM client can read monitors and incidents and take fenced, approval-gated actions over MCP, with one-click OAuth.",
            ),
            (
                "Do hosted and self-hosted share the same API?",
                "Yes. The data model, REST API and Terraform provider are identical whether you run the hosted tier or self-host.",
            ),
        ],
        "/mcp-server" => &[
            (
                "Which LLM clients work with it?",
                "Any Model Context Protocol client, including Claude, IDEs and the claude.ai connector. Connect with one-click OAuth or a scoped token.",
            ),
            (
                "Can the AI change my monitors?",
                "Only with your approval. Read tools cannot change anything, and each write action needs a scoped token plus your in-the-moment confirmation, and is audited.",
            ),
            (
                "What can it read?",
                "Org health, monitor lists and history, incident timelines, status pages and usage against your plan.",
            ),
            (
                "Is it safe from prompt injection?",
                "Customer-supplied text reaches the model labelled as data, never instructions, and no action runs without out-of-band human approval.",
            ),
        ],
        "/vs/pingdom" => &[
            (
                "Is Uptimepage free?",
                "Yes. The hosted tier is $0 a month with no credit card, and the AGPL source is free to self-host with unlimited monitors on your own hardware.",
            ),
            (
                "Does it include a status page?",
                "Yes. A branded status page on your own subdomain is part of the same product, with automatic incidents, maintenance windows, and email or webhook subscribers.",
            ),
            (
                "How often does it check?",
                "As often as every 60 seconds, across HTTP, TCP, DNS, TLS and ping, with the timing split across DNS, connect, TLS and first byte so you can see why a check is slow.",
            ),
            (
                "Can I manage it as code?",
                "Yes. An official Terraform provider, a full REST API, and an MCP server let you declare monitors in a repo and review changes in a pull request.",
            ),
        ],
        "/terraform-uptime-monitoring" => &[
            (
                "Which provider do I use?",
                "The official provider, source uptimepage/uptimepage on the Terraform Registry. It manages monitors, status pages, components and notification channels.",
            ),
            (
                "What can I declare in Terraform?",
                "Monitors with HTTP, TCP, DNS or TLS checks, public status pages and their components, and notification channels: the same things you change in the dashboard.",
            ),
            (
                "Do I need the hosted service?",
                "No. Start free on the hosted tier with no card, or self-host under AGPL and point the provider at your own instance.",
            ),
            (
                "How does the provider authenticate?",
                "With a scoped API token: resource-and-action permissions bound to one org, with an enforced expiry. Mint a write-scoped token for Terraform rather than an all-or-nothing key.",
            ),
        ],
        _ => &[],
    }
}

/// Decision matrix for `/open-source-uptime-monitoring`, verified July 2026
/// against each project's repo and docs. Uptime Kuma is the search default,
/// OpenStatus the nearest AGPL alternative, and Prometheus + Blackbox the
/// build-it-yourself route. Tones reflect reality, not favour: Kuma runs in
/// one container, which we do not.
static OPEN_SOURCE_MONITOR_MATRIX: Matrix = Matrix {
    heading: "open-source uptime monitors compared",
    columns: &[
        "Uptimepage",
        "Uptime Kuma",
        "OpenStatus",
        "Prometheus + Blackbox",
    ],
    rows: &[
        MatrixRow {
            label: "license",
            cells: &[
                ("AGPL", "yes"),
                ("MIT", "yes"),
                ("AGPL", "yes"),
                ("Apache 2.0", "yes"),
            ],
        },
        MatrixRow {
            label: "built with",
            cells: &[
                ("Rust", "yes"),
                ("JavaScript / Node", ""),
                ("TypeScript / Node", ""),
                ("Go", ""),
            ],
        },
        MatrixRow {
            label: "what you run",
            cells: &[
                ("binary + Postgres + ClickHouse", "part"),
                ("one container", "yes"),
                ("~6 Docker services", "part"),
                ("Prometheus + Alertmanager", "no"),
            ],
        },
        MatrixRow {
            label: "customer status page",
            cells: &[
                ("branded, subscribers", "yes"),
                ("basic, no subscribers", "part"),
                ("yes", "yes"),
                ("build it yourself", "no"),
            ],
        },
        MatrixRow {
            label: "monitoring as code",
            cells: &[
                ("Terraform · REST · MCP", "yes"),
                ("no REST API for monitors", "no"),
                ("Terraform · REST · MCP", "yes"),
                ("config files", "part"),
            ],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[
                ("yes, agents you run", "yes"),
                ("single instance", "no"),
                ("28 regions on hosted", "part"),
                ("federate it yourself", "part"),
            ],
        },
        MatrixRow {
            label: "teams and roles",
            cells: &[
                ("orgs + roles", "yes"),
                ("single shared login", "no"),
                ("yes", "yes"),
                ("external SSO / proxy", "part"),
            ],
        },
        MatrixRow {
            label: "auto incidents from checks",
            cells: &[
                ("yes", "yes"),
                ("basic", "part"),
                ("yes", "yes"),
                ("via Alertmanager", "part"),
            ],
        },
    ],
    notes: &[
        "Uptime Kuma is MIT; OpenStatus and Uptimepage are AGPL-3.0; Prometheus and Blackbox exporter are Apache 2.0.",
        "OpenStatus's 28-region checking is on its hosted tier; self-hosting runs several Docker services.",
        "Verified July 2026 against each project's repository and docs.",
    ],
};

/// Head-to-head facts for `/vs/self-hosted-status-pages`, verified in July
/// 2026 against each project's repository and, for Cachet, its live v3 source
/// (`cachethq/core`, whose docs lag the code). Cells are `(text, tone)`; the
/// first column is always Uptimepage. Refresh when a project ships.
static SELF_HOSTED_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "Upptime", "Cachet", "Statping"],
    rows: &[
        MatrixRow {
            label: "uptime checks built in",
            cells: &[
                ("yes", "yes"),
                ("yes", "yes"),
                ("basic HTTP", "part"),
                ("yes", "yes"),
            ],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("HTTP · TCP · DNS · TLS · ping", ""),
                ("HTTP · TCP · ICMP", ""),
                ("HTTP GET", ""),
                ("HTTP · TCP · UDP · ICMP · gRPC", ""),
            ],
        },
        MatrixRow {
            label: "fastest check interval",
            cells: &[
                ("60s", "yes"),
                ("5 min", "part"),
                ("your cron", "part"),
                ("~30s", "yes"),
            ],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[
                ("yes", "yes"),
                ("bolt-on", "part"),
                ("no", "no"),
                ("no", "no"),
            ],
        },
        MatrixRow {
            label: "status page",
            cells: &[
                ("branded", "yes"),
                ("static", ""),
                ("yes", "yes"),
                ("yes", "yes"),
            ],
        },
        MatrixRow {
            label: "visitor subscribers",
            cells: &[
                ("email + webhook", "yes"),
                ("no", "no"),
                ("webhook, email wip", "part"),
                ("no", "no"),
            ],
        },
        MatrixRow {
            label: "auto incidents from checks",
            cells: &[
                ("yes", "yes"),
                ("GitHub issues", ""),
                ("status only", "part"),
                ("yes", "yes"),
            ],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[
                ("Terraform · API · MCP", "yes"),
                ("YAML", ""),
                ("API", ""),
                ("YAML · API", ""),
            ],
        },
        MatrixRow {
            label: "official Terraform provider",
            cells: &[
                ("yes", "yes"),
                ("no", "no"),
                ("community", "part"),
                ("no", "no"),
            ],
        },
        MatrixRow {
            label: "tech stack",
            cells: &[
                ("Rust", ""),
                ("JavaScript", ""),
                ("PHP / Laravel", ""),
                ("Go", ""),
            ],
        },
        MatrixRow {
            label: "how you run it",
            cells: &[
                ("hosted or self-host", "yes"),
                ("Actions + Pages", ""),
                ("PHP/Laravel + DB", ""),
                ("Go binary", ""),
            ],
        },
        MatrixRow {
            label: "license",
            cells: &[
                ("AGPL-3.0", ""),
                ("MIT", ""),
                ("source-available", "part"),
                ("GPL-3.0", ""),
            ],
        },
        MatrixRow {
            label: "maintained in 2026",
            cells: &[
                ("active", "yes"),
                ("low velocity", "part"),
                ("active, v3 dev", "part"),
                ("~yearly", "part"),
            ],
        },
    ],
    notes: &[
        "Statping here is the maintained community fork, statping-ng; the original Statping has not shipped a release since 2020.",
        "Upptime runs checks as GitHub Actions cron jobs, which cannot fire more than once every five minutes and can drift later under load. Reaching other regions needs the third-party Globalping add-on.",
        "Cachet's actively developed v3 (the cachethq/core source) added basic HTTP component checks and subscriber management in mid-2026, but it is still 3.x-dev with no stable release: checks are HTTP GET only on a cron you add yourself, incident email to subscribers is not wired up yet, and the code ships under a custom source-available license.",
        "Competitor facts were verified against each project's repository and docs in July 2026. Open-source projects move quickly, so check their current docs before you decide.",
    ],
};

/// Head-to-head facts for `/vs/self-hosted-monitoring`, verified in July 2026
/// against each project's local source: Uptime Kuma 2.4.0, OpenStatus (HEAD
/// 2026-05), OneUptime 11.0.12, Gatus 5.36.0, Kener 4.1.1. Cells are
/// `(text, tone)`; the first column is always Uptimepage.
static MONITORING_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &[
        "Uptimepage",
        "Uptime Kuma",
        "OpenStatus",
        "OneUptime",
        "Gatus",
        "Kener",
    ],
    rows: &[
        MatrixRow {
            label: "fastest check interval",
            cells: &[
                ("10s", "yes"),
                ("20s", "yes"),
                ("30s", ""),
                ("60s", ""),
                ("seconds", "yes"),
                ("60s", ""),
            ],
        },
        MatrixRow {
            label: "check breadth",
            cells: &[
                ("HTTP·TCP·DNS·TLS·ping", ""),
                ("40+ types", "yes"),
                ("HTTP·TCP·DNS", ""),
                ("25+ types", "yes"),
                ("11 protocols", "yes"),
                ("9 types", ""),
            ],
        },
        MatrixRow {
            label: "ping / ICMP",
            cells: &[
                ("yes", "yes"),
                ("yes", "yes"),
                ("no", "no"),
                ("yes", "yes"),
                ("yes", "yes"),
                ("yes", "yes"),
            ],
        },
        MatrixRow {
            label: "TLS + domain expiry",
            cells: &[
                ("yes", "yes"),
                ("yes", ""),
                ("stub", "no"),
                ("yes", ""),
                ("yes", ""),
                ("SSL only", "part"),
            ],
        },
        MatrixRow {
            label: "push / heartbeat monitor",
            cells: &[
                ("no", "no"),
                ("yes", "yes"),
                ("no", "no"),
                ("yes", "yes"),
                ("yes", "yes"),
                ("yes", "yes"),
            ],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[
                ("yes", "yes"),
                ("basic", "part"),
                ("yes", ""),
                ("yes", ""),
                ("dashboard", "no"),
                ("yes", "yes"),
            ],
        },
        MatrixRow {
            label: "status-page subscribers",
            cells: &[
                ("email + webhook", "yes"),
                ("RSS only", "part"),
                ("email + webhook", ""),
                ("email·SMS·Slack", "yes"),
                ("no", "no"),
                ("email + RSS", ""),
            ],
        },
        MatrixRow {
            label: "alert channels",
            cells: &[
                ("14", ""),
                ("~95", "yes"),
                ("13", ""),
                ("9", ""),
                ("41", "yes"),
                ("4", "no"),
            ],
        },
        MatrixRow {
            label: "auto-incidents from checks",
            cells: &[
                ("yes", "yes"),
                ("no", "no"),
                ("yes", ""),
                ("yes", ""),
                ("no", "no"),
                ("yes", ""),
            ],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[
                ("yes", "yes"),
                ("add-on", "part"),
                ("28 regions", "yes"),
                ("yes", ""),
                ("no", "no"),
                ("no", "no"),
            ],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[
                ("Terraform · REST · MCP", "yes"),
                ("socket API", "no"),
                ("Terraform · REST · MCP", "yes"),
                ("Terraform · CLI", ""),
                ("YAML", "part"),
                ("REST API", "part"),
            ],
        },
        MatrixRow {
            label: "MCP server",
            cells: &[
                ("yes", "yes"),
                ("no", "no"),
                ("yes", ""),
                ("yes", ""),
                ("no", "no"),
                ("no", "no"),
            ],
        },
        MatrixRow {
            label: "teams / RBAC",
            cells: &[
                ("orgs + roles", "yes"),
                ("single user", "no"),
                ("workspaces", ""),
                ("projects + SSO", "yes"),
                ("basic auth", "no"),
                ("roles", ""),
            ],
        },
        MatrixRow {
            label: "tech stack",
            cells: &[
                ("Rust", ""),
                ("Node.js", ""),
                ("TypeScript + Go", ""),
                ("TypeScript", ""),
                ("Go", ""),
                ("SvelteKit", ""),
            ],
        },
        MatrixRow {
            label: "deploy footprint",
            cells: &[
                ("1 binary + 2 DBs", ""),
                ("1 container", "yes"),
                ("multi-service", "part"),
                ("6-14 services", "no"),
                ("1 tiny binary", "yes"),
                ("Node + Redis", ""),
            ],
        },
        MatrixRow {
            label: "license",
            cells: &[
                ("AGPL-3.0", ""),
                ("MIT", ""),
                ("AGPL-3.0", ""),
                ("Apache-2.0", ""),
                ("Apache-2.0", ""),
                ("MIT", ""),
            ],
        },
    ],
    notes: &[
        "Fastest interval each tool can reach; hosted free tiers are usually slower. Uptimepage's self-hosted floor is 10s, and hosted plans run at 60s on the free founding plan (with 50 monitors) or 30s on Pro.",
        "OpenStatus lists ICMP, UDP and SSL-certificate monitors in its config, but its open-source Go checker implements only HTTP, TCP and DNS.",
        "Uptime Kuma has around forty monitor types and about ninety-five alert integrations, but it is single-user, is configured over a socket API rather than REST or Terraform, and its status pages offer RSS, not email or webhook subscribers.",
        "Gatus is a health dashboard with badges rather than a subscriber status page, and its multi-region support is an experimental status-federation feature, not distributed probes.",
        "Alert-channel counts mix first-class and niche providers: Uptime Kuma's total includes the Apprise meta-provider and dozens of SMS gateways, and Gatus's includes automation bridges like Zapier, IFTTT and n8n. Uptimepage's fourteen are native integrations.",
        "Facts verified against each project's source in July 2026 (Uptime Kuma 2.4.0, OpenStatus, OneUptime 11.0.12, Gatus 5.36.0, Kener 4.1.1). Open-source projects move quickly, so check their current source before you decide.",
    ],
};

/// Head-to-head facts for `/vs/uptime-kuma`, verified in July 2026 against
/// Uptime Kuma 2.4.0 source. Cells are `(text, tone)`; column one is Uptimepage.
static UPTIME_KUMA_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "Uptime Kuma"],
    rows: &[
        MatrixRow {
            label: "fastest check interval",
            cells: &[("60s hosted · 10s self", "yes"), ("20s", "yes")],
        },
        MatrixRow {
            label: "check types",
            cells: &[("HTTP · TCP · DNS · TLS · ping", ""), ("40+ types", "yes")],
        },
        MatrixRow {
            label: "ping / ICMP",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "push / heartbeat monitor",
            cells: &[("no", "no"), ("yes", "yes")],
        },
        MatrixRow {
            label: "TLS + domain expiry",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[("yes", "yes"), ("basic", "part")],
        },
        MatrixRow {
            label: "status-page subscribers",
            cells: &[("email + webhook", "yes"), ("RSS only", "part")],
        },
        MatrixRow {
            label: "auto incidents from checks",
            cells: &[("yes", "yes"), ("manual", "no")],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[("yes", "yes"), ("add-on", "part")],
        },
        MatrixRow {
            label: "alert channels",
            cells: &[("14 native", ""), ("~95", "yes")],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[("Terraform · REST · MCP", "yes"), ("socket API", "no")],
        },
        MatrixRow {
            label: "teams / RBAC",
            cells: &[("orgs + roles", "yes"), ("single user", "no")],
        },
        MatrixRow {
            label: "how you run it",
            cells: &[("hosted or self-host", "yes"), ("1 container", "yes")],
        },
        MatrixRow {
            label: "license",
            cells: &[("AGPL-3.0", ""), ("MIT", "")],
        },
    ],
    notes: &[
        "Uptime Kuma is single-user with one shared login and is driven over an internal socket API rather than a REST or Terraform surface; its status pages offer an RSS feed, not email or webhook subscribers.",
        "Its ~95 integrations include the Apprise meta-provider and many SMS gateways; Uptimepage's 14 are native. Uptimepage has no passive heartbeat monitor yet.",
        "Verified against Uptime Kuma 2.4.0 source in July 2026. Open-source projects move quickly, so check the current source before you decide.",
    ],
};

/// Head-to-head facts for `/vs/oneuptime`, verified in July 2026 against
/// OneUptime 11.0.12 source. Cells are `(text, tone)`; column one is Uptimepage.
static ONEUPTIME_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "OneUptime"],
    rows: &[
        MatrixRow {
            label: "fastest check interval",
            cells: &[("60s hosted · 10s self", "yes"), ("60s", "")],
        },
        MatrixRow {
            label: "check types",
            cells: &[("HTTP · TCP · DNS · TLS · ping", ""), ("25+ types", "yes")],
        },
        MatrixRow {
            label: "ping / ICMP",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "push / heartbeat monitor",
            cells: &[("no", "no"), ("yes", "yes")],
        },
        MatrixRow {
            label: "TLS + domain expiry",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "status-page subscribers",
            cells: &[("email + webhook", "yes"), ("email · SMS · Slack", "yes")],
        },
        MatrixRow {
            label: "auto incidents from checks",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "on-call & escalation",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[("Terraform · REST · MCP", "yes"), ("Terraform · CLI", "")],
        },
        MatrixRow {
            label: "tech stack",
            cells: &[("Rust", ""), ("TypeScript", "")],
        },
        MatrixRow {
            label: "deploy footprint",
            cells: &[("1 binary + 2 DBs", "yes"), ("6-14 services", "no")],
        },
        MatrixRow {
            label: "license",
            cells: &[("AGPL-3.0", ""), ("Apache-2.0", "")],
        },
    ],
    notes: &[
        "OneUptime is a broad incident platform (monitoring, status pages, on-call, logs and tracing) that runs as 6-14 services with Postgres, ClickHouse and Redis; Uptimepage is a single binary with Postgres and ClickHouse.",
        "OneUptime adds heartbeat monitors and more check types, which Uptimepage does not have yet; Uptimepage runs a tighter footprint and a faster self-hosted interval.",
        "Verified against OneUptime 11.0.12 source in July 2026. Fast-moving project, so check the current source before you decide.",
    ],
};

/// Head-to-head facts for `/vs/uptimerobot`, verified against
/// uptimerobot.com/pricing in July 2026. Column one is Uptimepage.
static UPTIMEROBOT_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "UptimeRobot"],
    rows: &[
        MatrixRow {
            label: "fastest check interval",
            cells: &[
                ("60s hosted · 10s self", "yes"),
                ("5 min free · 60s paid", "part"),
            ],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("HTTP · TCP · DNS · TLS · ping", ""),
                ("HTTP · TCP · DNS · UDP · keyword", ""),
            ],
        },
        MatrixRow {
            label: "ping / ICMP",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "push / heartbeat monitor",
            cells: &[("no", "no"), ("yes", "yes")],
        },
        MatrixRow {
            label: "TLS + domain expiry",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "status-page subscribers",
            cells: &[("email + webhook", "yes"), ("email", "part")],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[("yes", "yes"), ("4 regions", "")],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[("Terraform · REST · MCP", "yes"), ("REST API", "part")],
        },
        MatrixRow {
            label: "self-host (open source)",
            cells: &[("yes · AGPL", "yes"), ("no", "no")],
        },
        MatrixRow {
            label: "free tier",
            cells: &[("free, no card", "yes"), ("50 monitors", "yes")],
        },
        MatrixRow {
            label: "price to start",
            cells: &[("$0", "yes"), ("paid from $7/mo", "")],
        },
    ],
    notes: &[
        "UptimeRobot's free plan allows 50 monitors but at a 5-minute interval with no login seats for teammates; 60-second checks, more status pages and team seats start on paid plans.",
        "UptimeRobot is a hosted service, not open-source or self-hostable, and exposes a REST API but no Terraform provider. Uptimepage adds a Terraform provider and MCP server but has no heartbeat monitor yet.",
        "Verified against uptimerobot.com/pricing in July 2026. SaaS plans change, so check their current pricing before you decide.",
    ],
};

/// Head-to-head facts for `/vs/better-stack`, verified against
/// betterstack.com/pricing in July 2026. Column one is Uptimepage.
static BETTER_STACK_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "Better Stack"],
    rows: &[
        MatrixRow {
            label: "fastest check interval",
            cells: &[("60s hosted · 10s self", "yes"), ("30s", "yes")],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("HTTP · TCP · DNS · TLS · ping", ""),
                ("HTTP · TCP · UDP · DNS · mail · ping", "yes"),
            ],
        },
        MatrixRow {
            label: "ping / ICMP",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "push / heartbeat monitor",
            cells: &[("no", "no"), ("1s heartbeat", "yes")],
        },
        MatrixRow {
            label: "TLS + domain expiry",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[("yes", "yes"), ("white-label", "yes")],
        },
        MatrixRow {
            label: "status-page subscribers",
            cells: &[("email + webhook", "yes"), ("1,000 included", "yes")],
        },
        MatrixRow {
            label: "on-call & escalation",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[("yes", "yes"), ("4 regions", "")],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[
                ("Terraform · REST · MCP", "yes"),
                ("Terraform · REST", "yes"),
            ],
        },
        MatrixRow {
            label: "teams / RBAC + SSO",
            cells: &[("orgs + roles", "yes"), ("RBAC + SSO", "yes")],
        },
        MatrixRow {
            label: "self-host (open source)",
            cells: &[("yes · AGPL", "yes"), ("no", "no")],
        },
        MatrixRow {
            label: "price to start",
            cells: &[("free, no card", "yes"), ("free 10 · paid $29/mo", "")],
        },
    ],
    notes: &[
        "Better Stack's free plan covers 10 monitors at 30-second checks with 1 status page; paid plans start around $29/month. It is a hosted service, not open-source or self-hostable.",
        "Better Stack has 1-second heartbeat monitors and broader check types that Uptimepage does not have yet. Uptimepage is AGPL and self-hostable, adds an MCP server, and starts free with no card.",
        "Verified against betterstack.com/pricing in July 2026. SaaS plans change, so check their current pricing before you decide.",
    ],
};

/// Head-to-head facts for `/vs/pingdom`, verified against pingdom.com and its
/// docs in July 2026. Column one is Uptimepage.
static PINGDOM_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "Pingdom"],
    rows: &[
        MatrixRow {
            label: "fastest check interval",
            cells: &[("60s hosted · 10s self", "yes"), ("60s", "")],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("HTTP · TCP · DNS · TLS · ping", ""),
                ("HTTP · TCP · UDP · DNS · ping · mail", "yes"),
            ],
        },
        MatrixRow {
            label: "ping / ICMP",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "transaction / real-user monitoring",
            cells: &[("no", "no"), ("yes", "yes")],
        },
        MatrixRow {
            label: "TLS + domain expiry",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "auto incidents from checks",
            cells: &[("yes", "yes"), ("alerts", "part")],
        },
        MatrixRow {
            label: "check locations",
            cells: &[("yes", "yes"), ("100+ locations", "yes")],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[("Terraform · REST · MCP", "yes"), ("REST API", "part")],
        },
        MatrixRow {
            label: "self-host (open source)",
            cells: &[("yes · AGPL", "yes"), ("no", "no")],
        },
        MatrixRow {
            label: "free tier",
            cells: &[("free, no card", "yes"), ("trial only", "no")],
        },
    ],
    notes: &[
        "Pingdom (by SolarWinds) has no free tier, only a 30-day trial; Uptimepage starts free with no card. Pingdom is a hosted service, not open-source or self-hostable.",
        "Pingdom checks from 100+ locations at a 1-minute minimum interval and adds transaction and real-user monitoring that Uptimepage does not have. Uptimepage pairs uptime with a status page in one binary, is AGPL, and adds a Terraform provider and MCP server.",
        "Verified against pingdom.com and its docs in July 2026. SaaS plans change, so check their current pricing before you decide.",
    ],
};

/// Head-to-head facts for `/vs/statuspage`, verified against
/// atlassian.com/software/statuspage/pricing in July 2026. Column one is
/// Uptimepage. Statuspage does not monitor — it is a status page only.
static STATUSPAGE_MATRIX: Matrix = Matrix {
    heading: "how they compare",
    columns: &["Uptimepage", "Statuspage"],
    rows: &[
        MatrixRow {
            label: "built-in uptime monitoring",
            cells: &[("yes", "yes"), ("no — needs 3rd-party", "no")],
        },
        MatrixRow {
            label: "check types",
            cells: &[("HTTP · TCP · DNS · TLS · ping", ""), ("none native", "no")],
        },
        MatrixRow {
            label: "auto incidents from checks",
            cells: &[("yes", "yes"), ("manual / integration", "part")],
        },
        MatrixRow {
            label: "branded status page",
            cells: &[("yes", "yes"), ("yes", "yes")],
        },
        MatrixRow {
            label: "custom domain",
            cells: &[("yes", "yes"), ("paid tiers", "")],
        },
        MatrixRow {
            label: "status-page subscribers",
            cells: &[
                ("email + webhook", "yes"),
                ("email · SMS · Slack · webhook", "yes"),
            ],
        },
        MatrixRow {
            label: "subscribers on free",
            cells: &[("included", "yes"), ("100", "part")],
        },
        MatrixRow {
            label: "scheduled maintenance",
            cells: &[("yes", "yes"), ("yes", "")],
        },
        MatrixRow {
            label: "config-as-code",
            cells: &[("Terraform · REST · MCP", "yes"), ("REST API", "part")],
        },
        MatrixRow {
            label: "teams / RBAC",
            cells: &[("orgs + roles", "yes"), ("RBAC on Business+", "")],
        },
        MatrixRow {
            label: "self-host (open source)",
            cells: &[("yes · AGPL", "yes"), ("no", "no")],
        },
        MatrixRow {
            label: "price to start",
            cells: &[("free, no card", "yes"), ("free · paid $29/mo", "")],
        },
    ],
    notes: &[
        "Atlassian Statuspage does not run its own checks — it is a status page only, fed by external monitors (Pingdom, Datadog, New Relic, Opsgenie). Uptimepage does the monitoring and the page in one.",
        "Statuspage's free plan includes 100 subscribers; the cheapest paid plan (Hobby) is $29/month with 250 subscribers and 5 team members. It is a hosted Atlassian product, not open-source or self-hostable.",
        "Verified against atlassian.com/software/statuspage/pricing in July 2026. Plans change, so check their current pricing before you decide.",
    ],
};

/// The comparison matrices, looked up by path so they stay off every other
/// `Landing`. Mirrors [`page_faqs`].
fn page_matrix(path: &str) -> Option<&'static Matrix> {
    match path {
        "/compare/openstatus-vs-uptime-kuma" => Some(&OPENSTATUS_KUMA_MATRIX),
        "/compare/uptime-kuma-vs-gatus" => Some(&KUMA_GATUS_MATRIX),
        "/compare/pingdom-vs-statuscake" => Some(&PINGDOM_STATUSCAKE_MATRIX),
        "/open-source-uptime-monitoring" => Some(&OPEN_SOURCE_MONITOR_MATRIX),
        "/vs/self-hosted-status-pages" => Some(&SELF_HOSTED_MATRIX),
        "/vs/self-hosted-monitoring" => Some(&MONITORING_MATRIX),
        "/vs/uptime-kuma" => Some(&UPTIME_KUMA_MATRIX),
        "/vs/oneuptime" => Some(&ONEUPTIME_MATRIX),
        "/vs/uptimerobot" => Some(&UPTIMEROBOT_MATRIX),
        "/vs/better-stack" => Some(&BETTER_STACK_MATRIX),
        "/vs/pingdom" => Some(&PINGDOM_MATRIX),
        "/vs/statuspage" => Some(&STATUSPAGE_MATRIX),
        _ => None,
    }
}

/// Third-party face-off for `/compare/openstatus-vs-uptime-kuma`, verified
/// July 2026 against each project's repository, docs and plan pages. The
/// Uptimepage column is last on purpose: the rivals are the subject.
static OPENSTATUS_KUMA_MATRIX: Matrix = Matrix {
    heading: "the facts, side by side",
    columns: &["OpenStatus", "Uptime Kuma", "Uptimepage"],
    rows: &[
        MatrixRow {
            label: "license",
            cells: &[("AGPL-3.0", ""), ("MIT", ""), ("AGPL-3.0", "")],
        },
        MatrixRow {
            label: "run it yourself",
            cells: &[
                ("multi-service TS stack", "part"),
                ("one container", "yes"),
                ("one binary + compose", "yes"),
            ],
        },
        MatrixRow {
            label: "hosted option",
            cells: &[
                ("yes, free tier", "yes"),
                ("no", "no"),
                ("yes, free tier", "yes"),
            ],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("HTTP · TCP · DNS, more in schema", ""),
                ("~40 incl. DBs · MQTT · browser", ""),
                ("HTTP · TCP · DNS · TLS · ping · domain", ""),
            ],
        },
        MatrixRow {
            label: "fastest interval",
            cells: &[
                ("30s on hosted paid tiers", ""),
                ("20s", ""),
                ("60s free · 30s Pro · 10s self-hosted", ""),
            ],
        },
        MatrixRow {
            label: "multi-region probes",
            cells: &[
                ("28 hosted regions", "yes"),
                ("single instance", "no"),
                ("multi-region, run your own", "yes"),
            ],
        },
        MatrixRow {
            label: "status page subscribers",
            cells: &[
                ("email · RSS", "yes"),
                ("none", "no"),
                ("email · webhook", "yes"),
            ],
        },
        MatrixRow {
            label: "config as code",
            cells: &[
                ("Terraform · REST · CLI · MCP", "yes"),
                ("UI only, no management API", "no"),
                ("Terraform · REST · MCP", "yes"),
            ],
        },
        MatrixRow {
            label: "teams & roles",
            cells: &[
                ("members on paid tiers", "yes"),
                ("single login", "no"),
                ("orgs + roles", "yes"),
            ],
        },
        MatrixRow {
            label: "community (GitHub stars)",
            cells: &[("~8.8k", ""), ("~89k", ""), ("young", "")],
        },
    ],
    notes: &[
        "OpenStatus's open-source checker implements HTTP, TCP and DNS; ICMP, UDP and TLS-certificate monitor types exist in its API schema.",
        "Star counts rounded from GitHub, July 2026.",
        "Verified July 2026 against each project's repository, documentation and plan pages. Refresh when a project ships.",
    ],
};

/// Third-party face-off for `/compare/uptime-kuma-vs-gatus`, verified July
/// 2026 against both repositories. Uptimepage column last: rivals first.
static KUMA_GATUS_MATRIX: Matrix = Matrix {
    heading: "the facts, side by side",
    columns: &["Uptime Kuma", "Gatus", "Uptimepage"],
    rows: &[
        MatrixRow {
            label: "license",
            cells: &[("MIT", ""), ("Apache-2.0", ""), ("AGPL-3.0", "")],
        },
        MatrixRow {
            label: "configuration",
            cells: &[
                ("UI only", ""),
                ("YAML only, read-only UI", ""),
                ("UI + Terraform + REST + MCP", ""),
            ],
        },
        MatrixRow {
            label: "run it yourself",
            cells: &[
                ("one container (Node)", "yes"),
                ("tiny static binary", "yes"),
                ("one binary + compose", "yes"),
            ],
        },
        MatrixRow {
            label: "hosted option",
            cells: &[
                ("no", "no"),
                ("gatus.io, paid", "part"),
                ("yes, free tier", "yes"),
            ],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("~40 incl. DBs · MQTT · browser", ""),
                ("11 protocols incl. gRPC · SSH · WebSocket", ""),
                ("HTTP · TCP · DNS · TLS · ping · domain", ""),
            ],
        },
        MatrixRow {
            label: "fastest interval",
            cells: &[
                ("20s", ""),
                ("no documented floor, default 60s", ""),
                ("60s free · 30s Pro · 10s self-hosted", ""),
            ],
        },
        MatrixRow {
            label: "status page",
            cells: &[
                ("yes, custom domains", "yes"),
                ("dashboard doubles as page", "part"),
                ("branded, own subdomain", "yes"),
            ],
        },
        MatrixRow {
            label: "page subscribers",
            cells: &[("none", "no"), ("none", "no"), ("email · webhook", "yes")],
        },
        MatrixRow {
            label: "incidents",
            cells: &[
                ("posted by hand", "part"),
                ("none", "no"),
                ("auto-opened from checks", "yes"),
            ],
        },
        MatrixRow {
            label: "teams & roles",
            cells: &[
                ("single login", "no"),
                ("one basic-auth or OIDC gate", "no"),
                ("orgs + roles", "yes"),
            ],
        },
        MatrixRow {
            label: "alert channels",
            cells: &[
                ("~95 services", ""),
                ("41 providers", ""),
                ("Slack · Telegram · PagerDuty · SMS + more", ""),
            ],
        },
        MatrixRow {
            label: "community (GitHub stars)",
            cells: &[("~89k", ""), ("~11.4k", ""), ("young", "")],
        },
    ],
    notes: &[
        "Kuma counts include the Apprise meta-provider and types implemented outside its monitor-types module; Gatus provider count from its README.",
        "Star counts rounded from GitHub, July 2026.",
        "Verified July 2026 against both repositories. Refresh when a project ships.",
    ],
};

/// Third-party face-off for `/compare/pingdom-vs-statuscake`, verified July
/// 2026 against both vendors' pricing/feature pages and Pingdom's API spec.
/// Prices are geo-localized by both vendors, so rows describe shape only.
static PINGDOM_STATUSCAKE_MATRIX: Matrix = Matrix {
    heading: "the facts, side by side",
    columns: &["Pingdom", "StatusCake", "Uptimepage"],
    rows: &[
        MatrixRow {
            label: "free tier",
            cells: &[
                ("no, 30-day trial", "no"),
                ("yes, 10 monitors @ 5 min", "yes"),
                ("yes, no card", "yes"),
            ],
        },
        MatrixRow {
            label: "pricing model",
            cells: &[
                ("usage ladders per product", ""),
                ("three tiers + add-ons", ""),
                ("free · founding · Pro", ""),
            ],
        },
        MatrixRow {
            label: "check types",
            cells: &[
                ("HTTP · TCP · ping · DNS · UDP · mail", ""),
                ("HTTP · TCP · DNS · SSH · SMTP · ping · push", ""),
                ("HTTP · TCP · DNS · TLS · ping · domain", ""),
            ],
        },
        MatrixRow {
            label: "fastest uptime interval",
            cells: &[
                ("1 min", ""),
                ("30s on top tier", ""),
                ("60s free · 30s Pro · 10s self-hosted", ""),
            ],
        },
        MatrixRow {
            label: "browser transactions + RUM",
            cells: &[
                ("yes, core products", "yes"),
                ("page speed only, no RUM", "part"),
                ("no", "no"),
            ],
        },
        MatrixRow {
            label: "probe locations",
            cells: &[
                ("~100 locations", ""),
                ("30+ countries", ""),
                ("EU · US · Asia-Pacific + run your own", ""),
            ],
        },
        MatrixRow {
            label: "status page",
            cells: &[
                ("included", "yes"),
                ("separate paid add-on", "part"),
                ("included, branded", "yes"),
            ],
        },
        MatrixRow {
            label: "page subscribers",
            cells: &[
                ("not published", ""),
                ("email · SMS, capped per add-on tier", "part"),
                ("email · webhook", "yes"),
            ],
        },
        MatrixRow {
            label: "open source / self-host",
            cells: &[("no", "no"), ("no", "no"), ("AGPL", "yes")],
        },
        MatrixRow {
            label: "team members",
            cells: &[
                ("unlimited on all plans", "yes"),
                ("capped per tier", "part"),
                ("orgs + roles", "yes"),
            ],
        },
    ],
    notes: &[
        "Pingdom check types from its public API spec; StatusCake types from its features pages.",
        "Both vendors geo-localize prices, so tiers are described by shape rather than numbers.",
        "Verified July 2026 against both vendors' public pages. Refresh when either changes plans.",
    ],
};

static RENDERED: OnceLock<HashMap<&'static str, CachedRender>> = OnceLock::new();

fn render_all(cfg: &MarketingCfg) -> HashMap<&'static str, CachedRender> {
    LANDINGS
        .iter()
        .map(|l| {
            let canonical_url = format!("{}{}", cfg.canonical_origin, l.path);
            let title = format!("{} | {BRAND}", l.title);
            let mut og = OpenGraph::default_for(&title, &canonical_url, &cfg.canonical_origin);
            og.description = l.meta_description.to_string();
            let faqs = page_faqs(l.path);
            let doc = LandingDoc {
                title,
                eyebrow: l.eyebrow,
                h1: l.h1,
                lede: l.lede,
                features: l.features,
                sections: l.sections,
                code: l.code.as_ref(),
                matrix: page_matrix(l.path),
                resources: l.resources,
                cta: l.cta,
                canonical_url,
                og,
                breadcrumb_json_ld: json_ld_breadcrumb(&cfg.canonical_origin, l.h1, l.path),
                software_json_ld: (!l.path.starts_with("/compare/"))
                    .then(|| json_ld_software_application(&cfg.canonical_origin)),
                webpage_json_ld: json_ld_webpage(
                    &cfg.canonical_origin,
                    l.path,
                    l.h1,
                    l.created,
                    l.lastmod,
                ),
                faq_json_ld: (!faqs.is_empty()).then(|| json_ld_faqpage(faqs)),
                faqs,
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
    // Old name for Better Stack; searchers still use it, alias 301s to the one page.
    r.route(
        "/vs/better-uptime",
        get(|| async { Redirect::permanent("/vs/better-stack") }),
    )
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
    fn every_landing_is_linked_in_footer() {
        let footer = include_str!("../../templates/marketing/base.html");
        for l in LANDINGS {
            assert!(
                footer.contains(&format!("href=\"{}\"", l.path)),
                "{} is orphaned: add it to the marketing footer",
                l.path
            );
        }
    }

    #[test]
    fn matrices_are_rectangular() {
        for l in LANDINGS {
            let Some(m) = page_matrix(l.path) else {
                continue;
            };
            assert!(
                m.us_col() < m.columns.len(),
                "{} matrix missing an Uptimepage column",
                l.path
            );
            for row in m.rows {
                assert_eq!(
                    row.cells.len(),
                    m.columns.len(),
                    "{} row {:?}: {} cells but {} columns",
                    l.path,
                    row.label,
                    row.cells.len(),
                    m.columns.len(),
                );
            }
        }
    }

    #[test]
    fn comparison_pages_carry_faqs() {
        for l in LANDINGS
            .iter()
            .filter(|l| l.path.starts_with("/vs/") || l.path.starts_with("/compare/"))
        {
            assert!(
                !page_faqs(l.path).is_empty(),
                "{} missing comparison FAQ",
                l.path
            );
        }
    }

    #[test]
    fn every_landing_has_seo_essentials() {
        for l in LANDINGS {
            assert!(!l.title.is_empty(), "{} missing title", l.path);
            let rendered_title = l.title.len() + " | ".len() + BRAND.len();
            assert!(
                rendered_title <= 60,
                "{} rendered <title> {} chars > 60",
                l.path,
                rendered_title
            );
            assert!(!l.h1.is_empty(), "{} missing h1", l.path);
            assert!(
                l.meta_description.len() <= 160,
                "{} meta description {} chars > 160",
                l.path,
                l.meta_description.len()
            );
            assert!(
                !l.features.is_empty() || page_matrix(l.path).is_some(),
                "{} has neither features nor a matrix",
                l.path
            );
            assert!(!l.sections.is_empty(), "{} missing sections", l.path);
        }
    }
}
