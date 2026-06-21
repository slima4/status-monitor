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

use super::config::{BRAND, MarketingCfg, TERRAFORM_URL};
use super::pages::{CachedRender, cached_render, serve_cached};
use super::seo::{
    JsonLd, OpenGraph, json_ld_breadcrumb, json_ld_faqpage, json_ld_software_application,
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
    pub code: Option<CodeSample>,
    pub resources: &'static [ResourceLink],
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
        resources: &[],
        cta: "Start free",
    },
    Landing {
        path: "/status-page-for-agencies",
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
        resources: &[],
        cta: "Start free",
    },
    Landing {
        path: "/open-source-status-page",
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
        ],
        cta: "Start free",
    },
    Landing {
        path: "/self-hosted-status-page",
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
        ],
        cta: "Start free",
    },
    Landing {
        path: "/pricing",
        title: "Pricing: Free, Every Feature",
        eyebrow: "pricing",
        h1: "One plan. Free. Every feature.",
        meta_description: "Uptimepage pricing: free. 20 monitors, 60-second checks, 90-day history, a branded status page and every alert channel, no credit card. Self-host AGPL too.",
        lede: "There is one plan and it is free: every feature on, no credit card, no per-seat or per-monitor metering. Here are the exact limits, and the self-hosted option with none.",
        features: &[
            Feature {
                label: "Price",
                value: "$0, no credit card",
            },
            Feature {
                label: "Monitors",
                value: "20",
            },
            Feature {
                label: "Check interval",
                value: "as fast as 60s",
            },
            Feature {
                label: "Check types",
                value: "HTTP, TCP, DNS, TLS",
            },
            Feature {
                label: "Public history",
                value: "90 days",
            },
            Feature {
                label: "Status page",
                value: "1, branded",
            },
            Feature {
                label: "Status components",
                value: "15",
            },
            Feature {
                label: "Team members",
                value: "3",
            },
            Feature {
                label: "Notification channels",
                value: "20",
            },
            Feature {
                label: "Alert channels",
                value: "Slack, email, SMS, webhook + more",
            },
            Feature {
                label: "Self-host",
                value: "AGPL, unlimited",
            },
        ],
        sections: &[
            Section {
                heading: "one free plan, the whole product",
                body: "Every feature is on for everyone: branded status pages, subscribers, incidents, scheduled maintenance, the REST API, Terraform and MCP. Nothing is gated behind a higher tier, there is no per-seat or per-monitor metering, and no credit card.",
            },
            Section {
                heading: "the limits, in plain numbers",
                body: "Twenty monitors, checks as fast as every 60 seconds, a 90-day public history, one branded status page with up to 15 components, and three team members. Generous for a personal project or a small team, and the same feature set whatever your size.",
            },
            Section {
                heading: "self-host for free, no limits",
                body: "Prefer to run it yourself? The source is AGPL. `docker compose up` brings up the binary with Postgres and ClickHouse, and you run as many monitors as your own hardware allows, with the same API and Terraform provider as the hosted tier.",
            },
        ],
        code: None,
        resources: &[
            ResourceLink {
                label: "Self-hosted status page",
                href: "/self-hosted-status-page",
            },
            ResourceLink {
                label: "Open-source status page",
                href: "/open-source-status-page",
            },
        ],
        cta: "Start free",
    },
    Landing {
        path: "/vs/uptimerobot",
        title: "An UptimeRobot Alternative with Built-in Status Pages",
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
        resources: &[],
        cta: "Start free",
    },
    Landing {
        path: "/vs/statuspage",
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
        resources: &[],
        cta: "Start free",
    },
    Landing {
        path: "/vs/better-stack",
        title: "A Better Stack Alternative You Can Self-Host",
        eyebrow: "comparing platforms",
        h1: "Looking for a Better Stack alternative?",
        meta_description: "Want monitoring and status pages you can self-host and drive as code? Uptimepage is one binary, AGPL, with a Terraform provider and MCP. Free to start.",
        lede: "Uptimepage is a focused monitor and status page you can run yourself. One binary, open source under AGPL, and everything you can click you can also declare in code. Start free on the hosted tier, no card.",
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
        resources: &[ResourceLink {
            label: "Monitoring as code",
            href: "/automation",
        }],
        cta: "Start free",
    },
    Landing {
        path: "/automation",
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
        path: "/mcp-server",
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
    resources: &'static [ResourceLink],
    cta: &'static str,
    canonical_url: String,
    og: OpenGraph,
    breadcrumb_json_ld: JsonLd,
    software_json_ld: JsonLd,
    faq_json_ld: Option<JsonLd>,
    faqs: &'static [(&'static str, &'static str)],
    app_url: String,
    version: &'static str,
}

/// Per-page FAQ for the landings that have one; others render no FAQ. Comparison
/// answers describe Uptimepage only, matching the neutral-comparison rule above.
fn page_faqs(path: &str) -> &'static [(&'static str, &'static str)] {
    match path {
        "/pricing" => &[
            (
                "Is Uptimepage really free?",
                "Yes. The hosted tier is $0 a month with every feature and no credit card. The AGPL source is also free to self-host.",
            ),
            (
                "What are the free-tier limits?",
                "20 monitors, checks as fast as every 60 seconds, 90 days of public history, one branded status page with up to 15 components, and three team members.",
            ),
            (
                "Is there a per-seat or per-monitor charge?",
                "No. One plan covers every feature with no metering. Nothing is gated behind a higher tier.",
            ),
            (
                "Is self-hosting free too?",
                "Yes. `docker compose up` runs the AGPL binary with Postgres and ClickHouse on your own boxes, with as many monitors as your hardware allows.",
            ),
        ],
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
        "/vs/uptimerobot" => &[
            (
                "Is Uptimepage free?",
                "Yes. The hosted tier is $0 a month with every feature and no credit card, and the AGPL source is free to self-host.",
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
                "Yes: $0 a month, every feature, no credit card, and no per-page pricing. Self-hosting under AGPL is free as well.",
            ),
            (
                "Can customers subscribe to updates?",
                "They can. Visitors opt in with confirmed email or webhook and hear about every incident and maintenance change, with signed payloads they can verify.",
            ),
        ],
        "/vs/better-stack" => &[
            (
                "Can I self-host Uptimepage?",
                "Yes. It ships as one AGPL binary with Postgres and ClickHouse, so `docker compose up` puts it live with your data on your own boxes.",
            ),
            (
                "Is there per-seat or per-monitor pricing?",
                "No. One free plan covers every feature, with no credit card and no per-seat or per-monitor metering.",
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
        _ => &[],
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
                resources: l.resources,
                cta: l.cta,
                canonical_url,
                og,
                breadcrumb_json_ld: json_ld_breadcrumb(&cfg.canonical_origin, l.h1, l.path),
                software_json_ld: json_ld_software_application(&cfg.canonical_origin),
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
