# Benchmarks

Criterion micro-benchmarks under `benches/`. Measure `execute_http_check` end-to-end through the same `hyper-util` client path the service uses in production.

```bash
cargo bench --bench http_client
cargo bench --bench public_status_ttfb   # requires `just up` (PG + CH)
```

> **Substrate caveat.** Every number on this page was captured on a developer
> laptop (Apple M1 Pro, 10 cores, 16 GB). Useful for **regression detection**
> across commits — **not** for production capacity planning. A real Linux
> server will outperform; a constrained VM will underperform. When sizing
> for production, re-run on the target topology.

## What the bench measures

| Bench | Unit |
|---|---|
| `http_check_single` | one `execute_http_check` call against in-process axum mock, h2c prior-knowledge |
| `http_check_throughput` | `c` concurrent calls via `join_all`, varying `c ∈ {100, 1000, 10000, 50000}` |

Each variant runs under two pinned topologies:

- **`1c`** — server + client share one OS thread (`current_thread` runtime). Single-core ceiling.
- **`2c`** — server on its own thread, client on the bench thread. Two-core ceiling.

Pinning makes results reproducible across machines: no `num_cpus()` drift.

## Single-core results (hyper-util, 2026-05-14)

M1 Pro, loopback h2c, mock returns `200 ok`:

| Bench | Latency (median) | Throughput | Δ vs reqwest baseline |
|---|---|---|---|
| `http_check_single/1c` | **37 µs** | 26.8 K rps | −21% latency · +17% rps |
| `http_check_throughput/1c/c_100` | 778 µs | **128 K rps** | −35% latency · **+54% rps** |
| `http_check_throughput/1c/c_1000` | 7.45 ms | **134 K rps** | −36% latency · **+56% rps** |
| `http_check_throughput/1c/c_10000` | 80.6 ms | 124 K rps | −30% latency · +44% rps |
| `http_check_throughput/1c/c_50000` | 422 ms | 118 K rps | −31% latency · +44% rps |

**One CPU sustains ~130 K checks/sec.** Per-check overhead at saturation = 1/130000 ≈ **7.7 µs**.

Saturation reached by `c=1000`. Larger concurrency = more wall time, same rps — bottleneck shifts to in-thread cooperative scheduling, not parallelism.

## Two-core results (hyper-util, 2026-05-14)

For comparison only — production CPU budget should be sized off `1c`.

| Bench | Latency (median) | Throughput |
|---|---|---|
| `http_check_single/2c` | 47.7 µs | 21 K rps |
| `http_check_throughput/2c/c_1000` | 6.52 ms | **153 K rps** |
| `http_check_throughput/2c/c_10000` | 76.7 ms | 130 K rps |
| `http_check_throughput/2c/c_50000` | 440 ms | 114 K rps |

Second core gains ~14% over `1c` at saturation. Single-check latency is *slower* on `2c` (48 µs vs 37 µs) — OS context-switch cost dominates when there's no parallelism to amortize.

## Public status page TTFB (50 orgs × 50 components)

`benches/public_status_ttfb.rs` provisions a 50-org × 50-component × 60-result fixture in PG + CH then times `LiveAggregator::build()` for one tenant.

| Metric | Value |
|---|---|
| Median | **14.0 ms** |
| 95% CI | 13.1–15.1 ms |
| Outliers | 6/40 (15%) — 3 high severe |
| Spec target (p99) | < 200 ms |

Comfortably under target — the `(org_id, target_id, ts)` ORDER BY on ClickHouse keeps single-tenant lookups bounded; no full-scan regression. Measures the aggregator only — full HTTP TTFB to the client adds template render + serialize + compression (~5–15 ms).

## Where the cycles go (historical — reqwest path)

Snapshot kept for context. samply, 15 s sample at `2c/c_10000` on the previous reqwest stack. The largest reqwest-specific cost — 7.5% on `url::parse` inside `reqwest::redirect::TowerRedirectPolicy` — disappeared with the hyper-util migration and explains a big chunk of the +44–56% throughput gain documented above.

| % of client thread | Cost | Notes |
|---|---|---|
| 7.5% | `url::parse` via `reqwest::redirect::TowerRedirectPolicy` | URL re-parsed per request even with `redirect::Policy::none()` — removed post-migration |
| 6.5% | `kevent` syscall | tokio io driver poll — inherent |
| 6.3% | `_platform_memmove` | h2 frame buffer copies — inherent |
| 5.0% | `mach_absolute_time` | tokio timer + criterion clock |
| 2.4% | `hyper_util::Client::send_request` | request dispatch |
| 1.5% | `h2::HeaderBlock::into_encoding` | HPACK encode |
| 1.5% | `pthread_mutex_lock` | hyper pool mutex |
| ~10% combined | h2 stream bookkeeping (pop/unlink/clone) | inherent to multiplexing |

## Methodology notes

- **`target_id`** is hoisted out of the iter — production uses fixed-per-target UUIDs, so paying `Uuid::now_v7`'s `getentropy` syscall per call would add ~10 µs of bench-only noise.
- **Mock returns `&'static str`** — no JSON, no allocation, no body parsing. Isolates client-side cost.
- **No TLS** — `verify_tls: false`, plain `http://`. TLS handshake amortizes over h2 connection reuse; not in this bench.
- **HTTP/2 prior-knowledge** (RFC 7540 §3.4) — single TCP connection multiplexes streams. Without it the bench would exhaust loopback ephemeral ports past `c≈10000` on macOS.
- **Loopback only**. Real network adds RTT (dominates everything here) plus DNS + TCP connect + TLS on first request per host.

## Reproducibility caveats

- macOS: no CPU isolation; Spotlight / Time Machine / runaway processes show as 5–10% outliers
- Linux: `taskset -c 0` pins the bench process to a single core for cleaner `1c` numbers
- Apple Silicon: P-core vs E-core scheduling is opaque; results can shift ~5% run-to-run

For production capacity planning use the **single-core throughput** above and multiply by your CPU budget. Empirical scaling stays sub-linear past `~4c` due to shared h2 connection state and pool mutex contention.
