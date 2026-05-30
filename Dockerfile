# syntax=docker/dockerfile:1.7

# Alpine builder = musl libc native → static binaries without crt-static gymnastics.
# cargo-chef splits dependency compile (slow, rarely invalidated) from app compile
# (fast, invalidated on every src change). Unchanged-deps rebuilds drop from ~5min
# to ~30s.
FROM rust:1-alpine AS chef
WORKDIR /usr/src/uptimepage
ENV CARGO_TERM_COLOR=never
# `curl` is needed at build time: utoipa-swagger-ui's build script shells out
# to it to fetch the Swagger UI assets bundle. `bash` runs scripts/fetch-
# tailwind.sh (which detects musl libc and downloads the matching upstream
# asset, so no glibc shim is required).
# `mold` = fast linker for the app/dep compile (cargo-chef already caches the
# dep layer, so sccache would be redundant here — and its cache mount isn't
# carried by `cache-to: type=gha`, so it'd be cold every CI run = pure tax).
# RUSTFLAGS is set in the `chef` base so every descendant compile stage
# inherits it (the build context doesn't COPY .cargo/).
RUN apk add --no-cache musl-dev pkgconfig curl bash mold
ENV RUSTFLAGS="-Clink-arg=-fuse-ld=mold"
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install cargo-chef --locked --version ^0.1

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /usr/src/uptimepage/recipe.json recipe.json
# Cook dependencies only — this layer caches as long as Cargo.toml/lock + the
# bench/bin shape don't change.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/uptimepage/target \
    cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY migrations ./migrations
COPY config ./config
COPY static ./static
COPY templates ./templates
# build.rs runs scripts/fetch-tailwind.sh then bakes static/css/app.css.
# legal.rs `include_str!`s the policy markdown + THIRD-PARTY-LICENSES.md at
# compile time, so those files must be in the build context here (the
# planner/cook stage stays deps-only — it never compiles the local crate).
COPY build.rs ./
COPY scripts ./scripts
COPY docs/legal ./docs/legal
COPY THIRD-PARTY-LICENSES.md ./
# AGPL-3.0 §13: bake the exact source identity so the running binary's
# footer offers the Corresponding Source. CI passes the commit/repo; an
# empty default lets a bare `docker build` fall back to build.rs's git
# probe (and to upstream) without failing.
ARG SM_SOURCE_COMMIT=
ARG SM_SOURCE_URL=
ENV SM_SOURCE_COMMIT=${SM_SOURCE_COMMIT} \
    SM_SOURCE_URL=${SM_SOURCE_URL}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/uptimepage/target \
    cargo build --release --bins && \
    mkdir -p /out && \
    cp target/release/uptimepage /out/uptimepage && \
    cp target/release/loadtest /out/loadtest

# distroless-static is ~2 MB and ships ca-certificates + /etc/passwd → enough for
# a static Rust binary that talks HTTPS via rustls-native-certs.
FROM gcr.io/distroless/static-debian12:nonroot
WORKDIR /app

COPY --from=builder /out/uptimepage /usr/local/bin/uptimepage
COPY --from=builder /out/loadtest /usr/local/bin/loadtest
COPY --from=builder /usr/src/uptimepage/config /app/config
COPY --from=builder /usr/src/uptimepage/migrations /app/migrations

ENV STATUS_MONITOR_SERVER__API_BIND=0.0.0.0:8080 \
    STATUS_MONITOR_SERVER__METRICS_BIND=0.0.0.0:9090

EXPOSE 8080 9090
USER nonroot
ENTRYPOINT ["/usr/local/bin/uptimepage"]
