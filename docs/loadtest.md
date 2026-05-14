# Load test

End-to-end harness. Spawns workers driving the production check executor against in-process mock servers. Different from the [micro-benchmarks](benchmarks.md), which measure single-call cost via Criterion.

```bash
cargo run --release --bin loadtest
```

## Linux verification (Docker)

50k concurrent runs need Linux kernel knobs that macOS doesn't expose. The compose stack ships a `loadtest` profile that runs the binary inside a Linux container with the required sysctls and ulimits:

```bash
docker compose --profile loadtest build loadtest
docker compose --profile loadtest run --rm loadtest

# override on the fly
docker compose --profile loadtest run --rm \
  -e CONCURRENCY=100000 -e DURATION_SECS=60 loadtest
```

The container sets `net.core.somaxconn=8192`, `net.ipv4.tcp_tw_reuse=1`, `net.ipv4.ip_local_port_range=10000 65535`, and `nofile=1048576` — none require `--privileged` since these sysctls are namespaced.

## Env

| Env | Default | Purpose |
|---|---|---|
| `CONCURRENCY` | `50000` | concurrent virtual workers |
| `DURATION_SECS` | `30` | how long to drive load |
| `TIMEOUT_MS` | `5000` | per-check request timeout |
| `MOCK_PORTS` | `16` | parallel in-process mock listeners — spreads 4-tuple load to avoid loopback ephemeral-port exhaustion |
| `RAMP_SECS` | `2` | worker start stagger window — avoids thundering-herd SYN bursts at `listen()` backlog |
| `HTTP2` | `0` | when `1`, client speaks HTTP/2 with prior knowledge (RFC 7540 §3.4). Single TCP connection multiplexes many streams; necessary to drive 50k workers on macOS where ephemeral src ports cap at ~16k |

## What it does

Spawns `MOCK_PORTS` axum servers returning `200 ok`, then drives workers in a tight loop using the same `build_clients` + check executor the production binary uses. Prints rolling RPS during the run and `total / success / rps / p50 / p95 / p99 / error-kind histogram` at the end.

## macOS notes

- `kern.ipc.somaxconn` caps listener backlog at **128** per socket (hard kernel limit)
- Ephemeral src port range: `49152–65535` = **16,384 ports**
- `TIME_WAIT` lingers 30 s, holding closed ports

For 50k-concurrency runs use `HTTP2=1` to fold many streams onto a few TCP connections. Linux defaults (ephemeral 32-61k, tunable `somaxconn`) handle 50k HTTP/1 natively.

## Reference numbers

> **Substrate caveat.** Every number below was captured on a developer
> laptop (Apple M1 Pro, 10 cores, 16 GB). Useful for **regression
> detection** ("did this change hurt the hot path?") and for **relative
> comparisons** between commits — **not** for production capacity
> planning. Treat them as floors, not ceilings: a real Linux host on
> server hardware will outperform; a constrained VM will underperform.
> When sizing for production, re-run on the target topology.

### macOS host (M1 Pro, 10 cores, loopback)

| Date | Config | Result |
|---|---|---|
| 2026-05-14 | `CONCURRENCY=50000 MOCK_PORTS=8 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` | **252,114 rps · 100% success · 75.7M checks · p50 181 ms · p95 283 ms · p99 393 ms** |
| earlier | `CONCURRENCY=50000 MOCK_PORTS=8 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` | 151,614 rps · 100% success · 45.5M checks · p99 579 ms |
| earlier | `CONCURRENCY=12000 MOCK_PORTS=24 RAMP_SECS=10 DURATION_SECS=300` (HTTP/1) | 27,894 rps · 99.79% success · p99 2.7 s |

The 2026-05-14 run is the current headline: 252 k rps sustained, p99 393 ms,
zero errors over 5 minutes. Captures the hot path with the multi-tenancy
work merged. Native macOS loopback on Darwin 25.4 reaches 50 k concurrent
HTTP/2 without the docker crutch — the older "macOS can't do 50 k loopback"
note in earlier docs is stale.

### Linux container (Docker Desktop VM on Mac)

| Date | Config | Result |
|---|---|---|
| 2026-05-14 | `CONCURRENCY=50000 MOCK_PORTS=16 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` (10 vCPU allocated) | 17,391 rps · 100% success · 5.25 M checks · p99 4.2 s · 26 timeouts |
| 2026-05-12 | `CONCURRENCY=50000 MOCK_PORTS=16 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` | 93,350 rps · 100% success · 28.1 M checks · p99 1.8 s · 933 MiB RSS peak |

The 2026-05-14 docker run regressed sharply versus the 2026-05-12 reference
on the same hardware. CPU was not the bottleneck (10 vCPU allocated and not
pegged); the regression sits in the Docker Desktop networking layer —
likely the `DOCKER_INSECURE_NO_IPTABLES_RAW` flag and iptables-rule changes
between DD versions. Same checkout's native run on the same box hit 252 k rps,
so the binary is fine; the VM substrate isn't.

Docker is no longer the right way to validate this binary's perf on macOS.
Prefer the native run above; reach for a real Linux host (CI runner, staging
VM) when you actually need a Linux number.

## HTTP/1 vs h2c trade-off

HTTP/1 exercises connect / pool churn — closer to "monitor checks N legacy endpoints" reality. h2c stresses HTTP/2 framing and flow control — closer to "monitor checks N gRPC / modern HTTPS endpoints with ALPN". Production monitors hit both. Default is HTTP/1; flip `HTTP2=1` when ephemeral exhaustion masks signal you actually care about.
