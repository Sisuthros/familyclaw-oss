# syntax=docker/dockerfile:1
#
# FamilyClaw gateway — Docker baseline (Layer A only).
#
# This image builds and runs ONLY the public, MIT-licensed gateway binary.
# It contains NO Layer B material: no SOUL.md, no calibration, no .env, no
# profiles/, no data/, no hearth/, no keys, no real names. All runtime
# configuration is supplied at container start via environment variables
# and/or volume-mounted directories (see FAMILYCLAW_PROFILE_DIR /
# FAMILYCLAW_DATA_DIR). NEVER bake secrets into this image.
#
# Crate / binary (verified from crates/familyclaw-gateway/Cargo.toml):
#   - package:  familyclaw-gateway
#   - [[bin]]:  familyclaw-gateway   (path = src/main.rs)
# The gateway crate already enables the living channel features
# (familyclaw-channels = { features = ["telegram", "discord"] }), so a plain
# release build of the gateway compiles those channels in — no extra
# --features flags are needed here.
#
# MSRV (verified from workspace Cargo.toml): rust-version = "1.88", edition 2021.

# ---------------------------------------------------------------------------
# Stage 1 — builder
# ---------------------------------------------------------------------------
# rust:1.88-bookworm matches the workspace MSRV (1.88) and gives a glibc
# (Debian bookworm) toolchain that links cleanly against the bookworm-slim
# runtime stage below.
FROM rust:1.88-bookworm AS builder

WORKDIR /build

# Copy the full workspace source. The .dockerignore excludes target/, .git/,
# docs, tests fixtures and ALL Layer B material, so only build inputs are
# sent to the daemon.
COPY . .

# Build only the gateway binary in release mode. The gateway crate's own
# Cargo.toml pulls in familyclaw-channels with the telegram + discord
# features, so this single command yields the "living" gateway.
#
# SECURITY FIX 2026-07-09 (audit [4], Layer 6): the wasmtime sandbox is OFF
# by default (large Cranelift+JIT dependency, increases build time/image
# size). Without it, 3rd-party skills fail closed (NoopSandbox =
# NotImplemented, does not run). Enable it once you register the first
# 3rd-party skill:
#   docker build --build-arg FAMILYCLAW_FEATURES=wasmtime ...
ARG FAMILYCLAW_FEATURES=""
RUN if [ -n "$FAMILYCLAW_FEATURES" ]; then \
      cargo build --release -p familyclaw-gateway --features "$FAMILYCLAW_FEATURES"; \
    else \
      cargo build --release -p familyclaw-gateway; \
    fi

# ---------------------------------------------------------------------------
# Stage 2 — runtime
# ---------------------------------------------------------------------------
# debian:bookworm-slim keeps the image small while still providing glibc and
# (after the install below) a TLS CA bundle so the OpenAI-compatible LLM
# client can make HTTPS calls. We also install wget for the HEALTHCHECK.
FROM debian:bookworm-slim AS runtime

# ca-certificates: required for outbound HTTPS (LLM provider endpoints).
# wget: used by the HEALTHCHECK below.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user where practical.
RUN useradd --system --create-home --uid 10001 familyclaw
USER familyclaw
WORKDIR /home/familyclaw

# Copy ONLY the built binary from the builder stage. No source, no Layer B,
# no secrets cross this boundary.
COPY --from=builder /build/target/release/familyclaw-gateway /usr/local/bin/familyclaw-gateway

# Bind on all interfaces inside the container's network namespace. The binary
# default is 127.0.0.1:8787 (loopback), which would be unreachable from
# outside the container; 0.0.0.0 makes the published port reachable. Override
# at runtime if you need a different bind address/port.
ENV FAMILYCLAW_GATEWAY_ADDR=0.0.0.0:8787

# Document the listening port (matches the default in FAMILYCLAW_GATEWAY_ADDR).
EXPOSE 8787

# Liveness probe against /healthz (no dependency checks — pure liveness).
# Uses wget, installed above.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://127.0.0.1:8787/healthz || exit 1

# Run the gateway in serve mode. `serve` is the default subcommand (kept for
# backwards compatibility), so an explicit `serve` is harmless and clear.
ENTRYPOINT ["/usr/local/bin/familyclaw-gateway"]
CMD ["serve"]
