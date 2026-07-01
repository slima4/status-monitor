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
        code: None,
        resources: &[
            ResourceLink {
                label: "Free pricing",
                href: "/pricing",
            },
            ResourceLink {
                label: "Versus Statuspage",
                href: "/vs/statuspage",
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
        ],
        cta: "Start free",
    },
    Landing {
        path: "/open-source-status-page",
        created: "2026-06-20",
        lastmod: "2026-06-20",
        title: "Open-Source Status Page with Built-in Monitoring",
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
                body: "Incidents open automatically from real HTTP, TCP, DNS and TLS checks and flow straight onto the page. There is no separate monitor to wire up and keep in sync.",
            },
            Section {
                heading: "open source, your way",
                body: "The source is AGPL. Run docker compose up with Postgres and ClickHouse on your own boxes, or start on the free hosted tier. The API and Terraform provider are the same either way.",
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
        ],
        cta: "Start free",
    },
    Landing {
        path: "/self-hosted-status-page",
        created: "2026-06-20",
        lastmod: "2026-06-20",
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
                body: "Run regional probe agents wherever your users are and fold their results into each monitor per region. Hosted or self-hosted, the data model, API and Terraform provider are identical.",
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
        lastmod: "2026-07-01",
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
                value: "HTTP, TCP, DNS, TLS",
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
                body: "Run regional probe agents on your own boxes and check from where your customers actually are. Each agent authenticates with a scoped, org-bound token.",
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
                label: "Best self-hosted monitors",
                href: "/blog/best-self-hosted-uptime-monitoring-tools",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/uptimerobot",
        created: "2026-06-16",
        lastmod: "2026-06-21",
        title: "An UptimeRobot Alternative with Status Pages",
        eyebrow: "switching monitors",
        h1: "Looking for an UptimeRobot alternative?",
        meta_description: "Comparing uptime monitors? Uptimepage pairs 60s HTTP, TCP, DNS and TLS checks with branded status pages and Slack, email and webhook alerts. Free to start.",
        lede: "If you are weighing your options, here is what Uptimepage gives you out of the box. Everything below is on the free tier, no card.",
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
                body: "HTTP, TCP, DNS and TLS, every minute. When something is slow, the timing is split across DNS, connect, TLS and time-to-first-byte, so you see why, not just that.",
            },
            Section {
                heading: "alerts tuned for humans",
                body: "Per-monitor Slack, email and webhook channels with dedupe and flap-suppression, so a brief blip doesn’t page anyone.",
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
        lastmod: "2026-06-21",
        title: "A Statuspage Alternative with Monitoring Built In",
        eyebrow: "switching status pages",
        h1: "Looking for a Statuspage alternative?",
        meta_description: "Uptimepage pairs a branded public status page with uptime monitoring in one product: 60s checks, email and webhook subscribers, incidents. Free to start.",
        lede: "Here the status page and the monitoring behind it are the same product. Flip any monitor public and customers get a branded page on your own subdomain. Everything below is on the free tier, no card.",
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
                heading: "keep customers in the loop",
                body: "Visitors subscribe for email or webhook updates and hear the moment an incident opens, updates, or resolves. Schedule maintenance windows ahead of time so planned work never reads as an outage.",
            },
            Section {
                heading: "branded, on your own subdomain",
                body: "Logo, colour, and a status URL on your subdomain. The page serves HTML for people and JSON plus RSS for machines, and stays up even when the backend behind it has a bad moment.",
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
        lastmod: "2026-07-01",
        title: "A Better Uptime (Better Stack) Alternative You Self-Host",
        eyebrow: "comparing platforms",
        h1: "Looking for a Better Uptime (Better Stack) alternative?",
        meta_description: "Better Uptime is now Better Stack. Want self-hosted monitoring and status pages you drive as code? Uptimepage is one AGPL binary with Terraform and MCP.",
        lede: "Better Uptime rebranded to Better Stack, and if you outgrew its pricing or want your data on your own boxes, Uptimepage is a focused monitor and status page you run yourself. One binary, open source under AGPL, and everything you can click you can also declare in code. Start free on the hosted tier, no card.",
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
                value: "HTTP, TCP, DNS, TLS",
            },
            Feature {
                label: "Price to start",
                value: "free, no card",
            },
        ],
        sections: &[
            Section {
                heading: "yours to run",
                body: "The whole thing ships as one self-contained binary. `docker compose up` brings up the monitor with Postgres and ClickHouse, migrations run on boot, and the source is AGPL if you’d rather host it on your own boxes.",
            },
            Section {
                heading: "everything as code",
                body: "Declare monitors, status pages and notification channels in HCL with the official Terraform provider, and point an LLM client at the MCP server to read your monitoring and take fenced, audited actions. No click-ops required.",
            },
            Section {
                heading: "probes where your users are",
                body: "Run region agents on your own machines and check from where your customers actually are. Each agent authenticates with a scoped, org-bound token.",
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
        path: "/vs/oneuptime",
        created: "2026-06-19",
        lastmod: "2026-06-20",
        title: "A OneUptime Alternative That’s Quick to Run",
        eyebrow: "comparing open source",
        h1: "Looking for a OneUptime alternative?",
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
                body: "One self-contained binary, Postgres for config and ClickHouse for the time-series. `docker compose up` and the whole stack is running with migrations applied. Nothing else to stand up first.",
            },
            Section {
                heading: "everything as code",
                body: "An official Terraform provider for monitors, status pages and channels, plus an MCP server so an LLM client can read your monitoring and take fenced, audited actions. Review your monitoring in a pull request.",
            },
            Section {
                heading: "hosted or self-hosted, your call",
                body: "Start on the free hosted tier with no card, or run the AGPL source yourself. The data model, API and Terraform provider are the same either way, so moving between them is just an endpoint change.",
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
        lastmod: "2026-06-20",
        title: "An Uptime Kuma Alternative You Run as Code",
        eyebrow: "comparing open source",
        h1: "Looking for an Uptime Kuma alternative?",
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
                body: "An official Terraform provider and a full REST API cover monitors, status pages and alert channels, and an MCP server lets an LLM client read your monitoring and take fenced, audited actions. Declare your monitoring in a repo and review changes in a pull request.",
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
                body: "Run regional probe agents on your own boxes, wherever your users are, and Uptimepage folds their results into each monitor's health per region. The data model, API and Terraform provider are identical hosted or self-hosted, so moving between them is just an endpoint change.",
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
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/pingdom",
        created: "2026-06-25",
        lastmod: "2026-06-25",
        title: "A Pingdom Alternative with Status Pages Built In",
        eyebrow: "switching monitors",
        h1: "Looking for a Pingdom alternative?",
        meta_description: "Uptimepage pairs 60s HTTP, TCP, DNS and TLS checks with branded status pages and Slack, email and webhook alerts. Open source, free to start.",
        lede: "If you are pricing out monitors, here is what Uptimepage gives you out of the box: the checks and a public status page are the same product, the source is open, and you can start free with no card.",
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
                heading: "checks that explain themselves",
                body: "HTTP, TCP, DNS and TLS, every minute from multiple regions. When something is slow the timing is split across DNS, connect, TLS and time-to-first-byte, so you see why, not just that.",
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
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/self-hosted-status-pages",
        created: "2026-07-01",
        lastmod: "2026-07-01",
        title: "Uptimepage vs Upptime, Cachet & Statping",
        eyebrow: "comparing self-hosted",
        h1: "Uptimepage vs Upptime, Cachet and Statping",
        meta_description: "How Uptimepage compares to Upptime, Cachet and Statping in 2026: built-in monitoring, 60-second checks, status pages, subscribers and config-as-code.",
        lede: "Three popular self-hosted status tools, one honest table. Upptime and Statping run their own checks; Cachet is a status page that has only recently, and partially, added checks of its own. Here is where each fits, and where Uptimepage does both jobs in one product. Start free on the hosted tier or self-host under AGPL, no card.",
        features: &[
            Feature {
                label: "Built-in checks",
                value: "HTTP, TCP, DNS, TLS",
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
                body: "Upptime is a neat idea. It runs checks as scheduled GitHub Actions, records history as commits in your repo, files incidents as GitHub Issues, and serves a static page from GitHub Pages. That design is also its ceiling. Actions cron will not run more than once every five minutes and can slip later under load, so detection is measured in minutes. There are no visitor subscriptions, checks run from a single region unless you add the third-party Globalping service, and there is no DNS-record or TLS-expiry check. Uptimepage runs its own checks every 60 seconds across HTTP, TCP, DNS and TLS from several regions, and lets visitors subscribe by email or webhook.",
            },
            Section {
                heading: "Cachet: a status page catching up on monitoring",
                body: "Cachet began as a pure communication tool: you set components up or down by hand or over its API. Its actively developed v3, in the cachethq/core repo, is moving fast and, as of mid-2026, added basic HTTP component checks and subscriber management. The checks are real but young: HTTP GET only, no TCP, DNS or TLS, and you schedule the check command yourself rather than getting a built-in interval. It is still 3.x-dev with no stable release, incident email to subscribers is not wired up yet, it is a PHP and Laravel app with a database, queue and cron to operate, and it ships under a custom source-available license rather than an OSI open-source one. Uptimepage runs HTTP, TCP, DNS and TLS checks every 60 seconds from multiple regions out of the box, opens incidents automatically, and is one binary to run.",
            },
            Section {
                heading: "Statping: close in shape, thin on upkeep",
                body: "Statping is the nearest match here. It is a single Go binary that runs its own HTTP, TCP, UDP, ICMP and gRPC checks, draws response-time graphs, and shows incidents and maintenance on a themeable page. The catch is upkeep. The original project stopped in 2020, and the community statping-ng fork carries it now at roughly one release a year, the most recent in mid-2025. It has no visitor subscriptions, no multi-region checks, and no Terraform provider. Uptimepage covers the same ground and adds config-as-code with Terraform, REST and MCP, team roles, subscriber pages and regional probes, hosted for free or self-hosted under AGPL.",
            },
            Section {
                heading: "One product, hosted or self-hosted",
                body: "The pattern is simple. Upptime and Statping monitor but leave out subscribers and multi-region; Cachet publishes but does not monitor. Uptimepage does both in one binary. Run docker compose up with Postgres and ClickHouse on your own boxes, or start free on the hosted tier with no card. The data model, REST API and Terraform provider are identical either way, so moving between them is just an endpoint change.",
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
        lastmod: "2026-07-01",
        title: "Uptimepage vs Uptime Kuma, OpenStatus, OneUptime, Gatus, Kener",
        eyebrow: "comparing self-hosted",
        h1: "Uptimepage vs the self-hosted monitoring tools",
        meta_description: "How Uptimepage compares to Uptime Kuma, OpenStatus, OneUptime, Gatus and Kener in 2026: checks, status pages, multi-region probes and config-as-code.",
        lede: "The modern self-hosted crowd, compared honestly. Uptime Kuma and Gatus check the most protocols and run the lightest; OpenStatus and OneUptime match Uptimepage on config-as-code and multi-region; Kener has the prettiest status page. Uptimepage sits where monitoring, a real subscriber status page and Terraform, REST and MCP meet in one binary. Start free on the hosted tier or self-host under AGPL, no card.",
        features: &[
            Feature {
                label: "Built-in checks",
                value: "HTTP, TCP, DNS, TLS, domain",
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
                body: "Uptime Kuma is the community favourite for good reason: around forty monitor types (databases, gRPC, MQTT, SNMP, Steam, real-browser, push heartbeats), roughly ninety-five alert integrations, one-second intervals, and a single container to run. Where it stops is the team and status-page side. It is single-user with no roles, it is driven entirely over a socket API with no REST or Terraform, its status pages take an RSS feed rather than email or webhook subscribers, and incidents are posted by hand, not opened from a failing check. Uptimepage trades some of that protocol breadth for a subscriber status page, organizations with roles, auto-opened incidents and config-as-code.",
            },
            Section {
                heading: "OpenStatus and OneUptime: the dev-first platforms",
                body: "These are the closest to Uptimepage in philosophy. OpenStatus is monitoring-as-code done well: a Terraform provider, a CLI, an MCP server, auto-resolving incidents, email and webhook subscribers, and probes across thirty-five regions with sub-minute checks. Its trade-offs are a heavier stack (Turso plus Tinybird plus hosted queues) and an open-source checker that implements only HTTP, TCP and DNS, with ICMP, UDP and SSL-certificate monitors declared in config but not built. OneUptime does everything Uptimepage does and then keeps going into on-call scheduling, escalation, logs, tracing and APM, but that reach costs you a Postgres, ClickHouse, Redis and many-service deployment to operate. Uptimepage aims at the same developer surface, Terraform, REST and MCP, but as one binary you can actually run. It matches those sub-minute checks too: 30 seconds on Pro and 10 seconds self-hosted, while the free founding plan already carries fifty monitors at sixty seconds.",
            },
            Section {
                heading: "Gatus: the protocol-rich checker",
                body: "Gatus is a joy if you want declarative checks in version control. Eleven endpoint protocols including gRPC, SSH, WebSocket, STARTTLS and UDP, a rich condition language with JSONPath body assertions and certificate-expiry checks, multi-step suites, and a tiny static binary with an optional zero-database mode. What it is not is a status page. It ships a health dashboard with badges, not a branded page with subscribers, it has no incident timeline, and it is single-tenant behind one basic-auth or OIDC boundary. Uptimepage covers the everyday HTTP, TCP, DNS and TLS checks and pairs them with the public status page, subscribers and multi-tenant teams Gatus leaves out.",
            },
            Section {
                heading: "Kener: the polished status page",
                body: "Kener is the best-looking status page of the group: separate light and dark palettes, custom CSS and footer HTML, twenty-four locales, embeddable widgets, four badge styles and custom RBAC roles. It checks real services too, including gRPC, SQL and heartbeats. The gaps are on the monitoring platform side: no multi-region probing, four alert channels (email, webhook, Slack, Discord), email-and-RSS subscribers only, a single tenant, and a hard Redis dependency. Uptimepage gives up a little status-page theming for multi-region probes, more alert channels and config-as-code.",
            },
            Section {
                heading: "Where Uptimepage fits",
                body: "Uptimepage is not the very fastest interval or the widest protocol list here, and it is honest about that. What it does is put the two halves together: real HTTP, TCP, DNS, TLS-certificate and domain-expiry monitoring, and a branded public status page with confirmed email and webhook subscribers, auto-opened incidents and scheduled maintenance. All of it is driven from code with a Terraform provider, a full REST API and an MCP server, isolated per organization with roles, and checked from probes you can run in any region. It runs as one binary with Postgres and ClickHouse, hosted for free or self-hosted under AGPL.",
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
    software_json_ld: JsonLd,
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
        "/open-source-status-page" => &[
            (
                "Is the status page really open source?",
                "Yes. Uptimepage is AGPL, so you can read the source, run it, and modify it. The hosted tier is $0 a month if you would rather not host it.",
            ),
            (
                "Does it monitor, or just publish?",
                "Both. Uptime monitoring is built in, so incidents open automatically from real HTTP, TCP, DNS and TLS checks and appear on the page without a second tool.",
            ),
            (
                "Can customers subscribe to updates?",
                "Yes. Visitors opt in with confirmed email or webhook and hear about every incident and maintenance change, with signed payloads they can verify.",
            ),
            (
                "Can I self-host it?",
                "Yes. `docker compose up` brings up the binary with Postgres and ClickHouse on your own boxes, with migrations applied on boot.",
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
                "Yes. Run regional probe agents on your own boxes and Uptimepage folds their results into each monitor per region.",
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
                "Yes. An MCP server lets an LLM client read your monitoring and take fenced, audited actions from the same config that lives in your repo.",
            ),
            (
                "Can I self-host it?",
                "Yes. `docker compose up` brings up the single AGPL binary with Postgres and ClickHouse, and migrations run on boot.",
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
                "Yes. `docker compose up` brings up the single binary with Postgres and ClickHouse, and migrations run on boot.",
            ),
        ],
        "/vs/statuspage" => &[
            (
                "Does Uptimepage monitor as well as publish?",
                "Yes. Uptime monitoring is built in, so incidents open automatically from real HTTP, TCP, DNS and TLS checks and flow straight onto the status page.",
            ),
            (
                "Is a custom domain included?",
                "Every org gets a branded subdomain out of the box, and a custom CNAME is on the way. Branding, logo and colours are included, not gated behind a higher tier.",
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
                "Yes. It ships as one AGPL binary with Postgres and ClickHouse, so `docker compose up` puts it live with your data on your own boxes.",
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
                "As of mid-2026, partly. Cachet v3 added basic HTTP checks you schedule yourself (GET only, no TCP, DNS or TLS), though it is still in development with no stable release. Uptimepage runs HTTP, TCP, DNS and TLS checks every 60 seconds from multiple regions and opens incidents automatically.",
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
                "Yes. `docker compose up` brings up the single AGPL binary with Postgres and ClickHouse, migrations run on boot, and the hosted tier is free if you would rather not run it.",
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
                "Yes. `docker compose up` brings up the single AGPL binary with Postgres and ClickHouse, migrations run on boot, and the hosted tier is free if you would rather not run it.",
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
                "As often as every 60 seconds, across HTTP, TCP, DNS and TLS, with the timing split across DNS, connect, TLS and first byte so you can see why a check is slow.",
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
                ("HTTP · TCP · DNS · TLS", ""),
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
                ("1s", "yes"),
                ("30s", ""),
                ("60s", ""),
                ("seconds", "yes"),
                ("60s", ""),
            ],
        },
        MatrixRow {
            label: "check breadth",
            cells: &[
                ("HTTP·TCP·DNS·TLS", ""),
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
                ("no", "no"),
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
                ("35 regions", "yes"),
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
                ("Terraform · CLI", ""),
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

/// The comparison matrices, looked up by path so they stay off every other
/// `Landing`. Mirrors [`page_faqs`].
fn page_matrix(path: &str) -> Option<&'static Matrix> {
    match path {
        "/vs/self-hosted-status-pages" => Some(&SELF_HOSTED_MATRIX),
        "/vs/self-hosted-monitoring" => Some(&MONITORING_MATRIX),
        _ => None,
    }
}

static RENDERED: OnceLock<HashMap<&'static str, CachedRender>> = OnceLock::new();

fn render_all(cfg: &MarketingCfg) -> HashMap<&'static str, CachedRender> {
    LANDINGS
        .iter()
        .map(|l| {
            let canonical_url = format!("{}{}", cfg.canonical_origin, l.path);
            let title = format!("{} | {BRAND}", l.title);
            let mut og = OpenGraph::default_for(&title, &canonical_url);
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
                software_json_ld: json_ld_software_application(&cfg.canonical_origin),
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
        for l in LANDINGS.iter().filter(|l| l.path.starts_with("/vs/")) {
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
