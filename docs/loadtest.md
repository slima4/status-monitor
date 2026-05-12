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

### macOS host (M-class Mac, loopback)

| Config | Result |
|---|---|
| `CONCURRENCY=12000 MOCK_PORTS=24 RAMP_SECS=10 DURATION_SECS=300` (HTTP/1) | 27,894 rps · 99.79% success · p99 2.7 s |
| `CONCURRENCY=50000 MOCK_PORTS=8 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` | **131,209 rps · 100% success · 39.4M checks · p99 769 ms** |

### Linux container (Docker Desktop VM on Mac)

| Config | Result |
|---|---|
| `CONCURRENCY=50000 MOCK_PORTS=16 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` | **93,350 rps · 100% success · 28.1M checks · p99 1.8 s** |

The 50k-concurrent acceptance run requires Linux kernel knobs (somaxconn, tw_reuse, ip_local_port_range) — see the Docker section above. Pure-Linux numbers will be higher than VM-on-Mac (~30% hypervisor overhead).

## HTTP/1 vs h2c trade-off

HTTP/1 exercises connect / pool churn — closer to "monitor checks N legacy endpoints" reality. h2c stresses HTTP/2 framing and flow control — closer to "monitor checks N gRPC / modern HTTPS endpoints with ALPN". Production monitors hit both. Default is HTTP/1; flip `HTTP2=1` when ephemeral exhaustion masks signal you actually care about.
