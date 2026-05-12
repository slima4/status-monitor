# syntax=docker/dockerfile:1.7

# Alpine builder = musl libc native → static binaries without crt-static gymnastics.
FROM rust:1-alpine AS builder
WORKDIR /usr/src/status-monitor

ENV CARGO_TERM_COLOR=never

RUN apk add --no-cache musl-dev pkgconfig

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY migrations ./migrations
COPY config ./config

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/status-monitor/target \
    cargo build --release --bins && \
    mkdir -p /out && \
    cp target/release/status-monitor /out/status-monitor && \
    cp target/release/loadtest /out/loadtest

# distroless-static is ~2 MB and ships ca-certificates + /etc/passwd → enough for
# a static Rust binary that talks HTTPS via rustls-native-certs.
FROM gcr.io/distroless/static-debian12:nonroot
WORKDIR /app

COPY --from=builder /out/status-monitor /usr/local/bin/status-monitor
COPY --from=builder /out/loadtest /usr/local/bin/loadtest
COPY --from=builder /usr/src/status-monitor/config /app/config
COPY --from=builder /usr/src/status-monitor/migrations /app/migrations

ENV STATUS_MONITOR_SERVER__API_BIND=0.0.0.0:8080 \
    STATUS_MONITOR_SERVER__METRICS_BIND=0.0.0.0:9090

EXPOSE 8080 9090
USER nonroot
ENTRYPOINT ["/usr/local/bin/status-monitor"]
