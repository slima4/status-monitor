# uptimepage

Async Rust service that runs HTTP and TCP health checks against a configurable set of targets, applies per-host circuit breaking, batches results, and ships them to durable storage. Targets persist in PostgreSQL; check results land in ClickHouse for high-cardinality time-series queries. Exposes a REST API for target CRUD and result queries, a server-rendered operator UI on the same port, and Prometheus metrics on a separate port.

Built on Rust 1.95 (edition 2024), Tokio, Axum, hyper-util (custom phase-timing connector + tokio-rustls), sqlx, and the official `clickhouse` crate. UI layer uses askama 0.16 + HTMX 2 + Tailwind 4 + ECharts 6, all served from the same binary. Designed for low-overhead checks at ~50k concurrent in-flight.

## Where to start

- New to the project → [Architecture](architecture.md) for the big picture
- Integrating → [REST API](api.md), or the [MCP server](mcp.md) for LLM clients
- Browsing the data → [Web UI](ui.md)
- Getting alerted → [Notifications](notifications.md)
- Running it → [Deployment](deployment.md) and [Configuration](configuration.md)
- Operating it → [Metrics & tracing](metrics.md) and [Troubleshooting](troubleshooting.md)
- Benchmarking → [Benchmarks](benchmarks.md) (per-check micro) and [Load test](loadtest.md) (end-to-end)

## Source

[github.com/uptimepage/uptimepage](https://github.com/uptimepage/uptimepage)
