# syntax=docker/dockerfile:1.7

# Alpine builder = musl libc native → static binaries without crt-static gymnastics.
# cargo-chef splits dependency compile (slow, rarely invalidated) from app compile
# (fast, invalidated on every src change). Unchanged-deps rebuilds drop from ~5min
# to ~30s.
FROM rust:1-alpine AS chef
WORKDIR /usr/src/status-monitor
ENV CARGO_TERM_COLOR=never
# `curl` is needed at build time: utoipa-swagger-ui's build script shells out
# to it to fetch the Swagger UI assets bundle. `bash` runs scripts/fetch-
# tailwind.sh (which detects musl libc and downloads the matching upstream
# asset, so no glibc shim is required).
RUN apk add --no-cache musl-dev pkgconfig curl bash
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install cargo-chef --locked --version ^0.1

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /usr/src/status-monitor/recipe.json recipe.json
# Cook dependencies only — this layer caches as long as Cargo.toml/lock + the
# bench/bin shape don't change.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/status-monitor/target \
    cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY migrations ./migrations
COPY config ./config
COPY static ./static
COPY templates ./templates
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
