# syntax=docker/dockerfile:1.7

FROM rust:1.95-bookworm AS builder
WORKDIR /usr/src/status-monitor

ENV CARGO_TERM_COLOR=never

RUN apt-get update && \
    apt-get install -y --no-install-recommends pkg-config ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY migrations ./migrations
COPY config ./config

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/status-monitor/target \
    cargo build --release --bins && \
    cp target/release/status-monitor /usr/local/bin/status-monitor && \
    cp target/release/loadtest /usr/local/bin/loadtest

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app

COPY --from=builder /usr/local/bin/status-monitor /usr/local/bin/status-monitor
COPY --from=builder /usr/local/bin/loadtest /usr/local/bin/loadtest
COPY --from=builder /usr/src/status-monitor/config /app/config
COPY --from=builder /usr/src/status-monitor/migrations /app/migrations

ENV STATUS_MONITOR_SERVER__API_BIND=0.0.0.0:8080 \
    STATUS_MONITOR_SERVER__METRICS_BIND=0.0.0.0:9090

EXPOSE 8080 9090
USER nonroot
ENTRYPOINT ["/usr/local/bin/status-monitor"]
