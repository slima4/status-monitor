# Benchmarks

Criterion micro-benchmarks under `benches/`. Measure `execute_http_check` end-to-end through the same `reqwest` client path the service uses in production.

```bash
cargo bench --bench http_client
```

## What the bench measures

| Bench | Unit |
|---|---|
| `http_check_single` | one `execute_http_check` call against in-process axum mock, h2c prior-knowledge |
| `http_check_throughput` | `c` concurrent calls via `join_all`, varying `c ∈ {100, 1000, 10000, 50000}` |

Each variant runs under two pinned topologies:

- **`1c`** — server + client share one OS thread (`current_thread` runtime). Single-core ceiling.
- **`2c`** — server on its own thread, client on the bench thread. Two-core ceiling.

Pinning makes results reproducible across machines: no `num_cpus()` drift.

## Single-core results

M-class Mac, loopback h2c, mock returns `200 ok`:

| Bench | Latency (median) | Throughput |
|---|---|---|
| `http_check_single/1c` | **47 µs** | 22.8 K rps |
| `http_check_throughput/1c/c_100` | 1.20 ms | 83 K rps |
| `http_check_throughput/1c/c_1000` | 11.6 ms | 86 K rps |
| `http_check_throughput/1c/c_10000` | 116 ms | 86 K rps |
| `http_check_throughput/1c/c_50000` | 608 ms | 82 K rps |

**One CPU sustains ~85 K checks/sec.** Per-check overhead at saturation = 1/85000 ≈ **12 µs**.

Saturation reached by `c=1000`. Larger concurrency = more wall time, same rps — bottleneck shifts to in-thread cooperative scheduling, not parallelism.

## Two-core reference

For comparison only — production CPU budget should be sized off `1c`.

| Bench | Throughput |
|---|---|
| `http_check_single/2c` | 18.4 K rps (53 µs/call) |
| `http_check_throughput/2c/c_1000` | 104 K rps |
| `http_check_throughput/2c/c_10000` | 116 K rps |
| `http_check_throughput/2c/c_50000` | 105 K rps |

Second core gains ~25% over `1c`. Single-check latency is *slower* on `2c` (53 µs vs 47 µs) — OS context-switch cost dominates when there's no parallelism to amortize.

## Where the cycles go

Profile (samply, 15 s sample at `2c/c_10000`):

| % of client thread | Cost | Notes |
|---|---|---|
| 7.5% | `url::parse` via `reqwest::redirect::TowerRedirectPolicy` | URL re-parsed per request even with `redirect::Policy::none()` |
| 6.5% | `kevent` syscall | tokio io driver poll — inherent |
| 6.3% | `_platform_memmove` | h2 frame buffer copies — inherent |
| 5.0% | `mach_absolute_time` | tokio timer + criterion clock |
| 2.4% | `hyper_util::Client::send_request` | request dispatch |
| 1.5% | `h2::HeaderBlock::into_encoding` | HPACK encode |
| 1.5% | `pthread_mutex_lock` | hyper pool mutex |
| ~10% combined | h2 stream bookkeeping (pop/unlink/clone) | inherent to multiplexing |

The 7.5% on URL parsing inside reqwest's redirect middleware is the largest avoidable cost. Reaching it requires bypassing reqwest's middleware stack — out of scope for v1, tracked in deferred work.

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
