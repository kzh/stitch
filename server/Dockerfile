# syntax=docker/dockerfile:1.8
###############################################################################
# 0. Base image with shared build tooling
###############################################################################
ARG RUST_VERSION=1.87.0
###############################################################################
# Tools – compile heavy Rust CLI utilities once
###############################################################################
FROM rust:${RUST_VERSION}-slim AS tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,id=rust-tools-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=rust-tools-target,target=/usr/local/cargo/target \
    CARGO_BUILD_JOBS=$(nproc) cargo install --locked cargo-chef sccache

###############################################################################
# 0. Base image with shared build tooling
###############################################################################
FROM rust:${RUST_VERSION}-slim AS base
RUN apt-get update && \
    apt-get install -y --no-install-recommends pkg-config libssl-dev protobuf-compiler && \
    rm -rf /var/lib/apt/lists/*
COPY --from=tools /usr/local/cargo/bin/ /usr/local/cargo/bin/
ENV RUSTC_WRAPPER=sccache \
    CARGO_PROFILE_RELEASE_STRIP=symbols \
    SCCACHE_DIR=/sccache \
    RUSTFLAGS="--cfg tokio_unstable"

###############################################################################
# 1. Planner – compute dependency graph
###############################################################################
FROM base AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

###############################################################################
# 2. Cacher – build dependencies only (leverages registry cache)
###############################################################################
FROM base AS cacher
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    CARGO_BUILD_JOBS=$(nproc) cargo chef cook --release --recipe-path recipe.json --locked

###############################################################################
# 3. Builder – compile application
###############################################################################
FROM base AS builder
WORKDIR /app
COPY --from=cacher /usr/local/cargo /usr/local/cargo
COPY --from=cacher /app/target /app/target
COPY . .
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    cargo build --release --bin stitch-server --locked -j $(nproc)

###############################################################################
# 4. Runtime – tiny, non-root distroless image
###############################################################################
FROM gcr.io/distroless/cc:nonroot
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /app/target/release/stitch-server /stitch-server
LABEL org.opencontainers.image.source="https://github.com/kzh/stitch"
EXPOSE 50051 50052
ENTRYPOINT ["/stitch-server"]