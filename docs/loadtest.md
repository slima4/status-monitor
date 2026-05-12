# Load test

```bash
cargo run --release --bin loadtest
```

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

## Reference numbers (M-class Mac, single mock host)

| Config | Result |
|---|---|
| `CONCURRENCY=12000 MOCK_PORTS=24 RAMP_SECS=10 DURATION_SECS=300` (HTTP/1) | 27,894 rps · 99.79% success · p99 2.7 s |
| `CONCURRENCY=50000 MOCK_PORTS=8 RAMP_SECS=10 HTTP2=1 DURATION_SECS=300` | **131,209 rps · 100% success · 39.4M checks · p99 769 ms** |

## HTTP/1 vs h2c trade-off

HTTP/1 exercises connect / pool churn — closer to "monitor checks N legacy endpoints" reality. h2c stresses HTTP/2 framing and flow control — closer to "monitor checks N gRPC / modern HTTPS endpoints with ALPN". Production monitors hit both. Default is HTTP/1; flip `HTTP2=1` when ephemeral exhaustion masks signal you actually care about.
