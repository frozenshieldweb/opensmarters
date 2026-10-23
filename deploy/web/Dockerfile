# syntax=docker/dockerfile:1
# SPDX-License-Identifier: GPL-3.0-or-later
#
# opensmarters web: the self-hosted web edition (docs/WEB_DEPLOY.md).
# Build context: the repository root.
#
#   docker build -f deploy/web/Dockerfile -t opensmarters-web .
#   docker run --rm opensmarters-web --generate-secret-key
#   docker run -d -p 8080:8080 -v opensmarters-data:/data \
#     -e OPENSMARTERS_SECRET_KEY=<key> opensmarters-web
#
# Stage 1 builds the browser UI (web/) when the repository has one, stage 2 builds the
# server with the pinned Rust toolchain and embeds the UI, stage 3 is a slim Debian
# runtime with ffmpeg, running as an unprivileged user under tini.

ARG NODE_IMAGE=node:22-trixie-slim
ARG RUST_IMAGE=rust:1.99.1-slim-trixie
ARG RUNTIME_IMAGE=debian:trixie-slim

# --- 1. The browser UI ---------------------------------------------------------------
FROM ${NODE_IMAGE} AS ui
WORKDIR /build
# The whole context is bind-mounted (read-only, .dockerignore applies) so that a
# checkout without web/ still builds: the server then serves its built-in page that
# says the UI isn't built.
RUN --mount=type=bind,source=.,target=/context \
    mkdir -p /out/web/dist \
 && if [ -f /context/web/package.json ]; then \
      cp -R /context/web /build/web \
      && cd /build/web \
      && if [ -f package-lock.json ]; then npm ci --no-audit --no-fund; else npm install --no-audit --no-fund; fi \
      && npm run build \
      && cp -R dist/. /out/web/dist/ ; \
    else \
      echo "web/package.json not found: building the server without the UI"; \
    fi

# --- 2. The server -------------------------------------------------------------------
FROM ${RUST_IMAGE} AS server
WORKDIR /src
COPY . .
# rust-toolchain.toml pins the compiler (and its components); rustup installs exactly
# that toolchain, so the image tag above only saves a download when it matches.
RUN rustup toolchain install && rustup show active-toolchain
COPY --from=ui /out/web/dist /src/web/dist
# Optional: how many compilers run at once. Cargo uses every CPU by default; each job can
# take about 1 GB, so a server with many cores and little memory should set this
# (Dokploy: Environment → Build Time Arguments, e.g. CARGO_BUILD_JOBS=2).
ARG CARGO_BUILD_JOBS
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build -p opensmarters-web --release --locked ${CARGO_BUILD_JOBS:+--jobs "$CARGO_BUILD_JOBS"} \
 && install -D -m 0755 target/release/opensmarters-web /out/opensmarters-web

# --- 3. The runtime ------------------------------------------------------------------
FROM ${RUNTIME_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 opensmarters \
 && useradd --system --uid 10001 --gid 10001 --home-dir /data --no-create-home \
      --shell /usr/sbin/nologin opensmarters \
 && mkdir -p /data \
 && chown 10001:10001 /data \
 && chmod 0700 /data
COPY --from=server /out/opensmarters-web /usr/local/bin/opensmarters-web
ENV OPENSMARTERS_DATA_DIR=/data \
    OPENSMARTERS_LISTEN=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080
USER 10001:10001
WORKDIR /data
HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/opensmarters-web", "--healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/opensmarters-web"]
