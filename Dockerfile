# Lemmate — multi-stage image: Svelte web client + lemmate-server + lemmate CLI.
#
# Build:  docker build -t lemmate .
# Run:    docker run -p 8080:8080 -v lemmate_data:/data lemmate
#
# The workspace also contains the two Tauri 2 shells, crates/desktop and crates/mobile, which
# need webkit2gtk/GTK and an Android NDK respectively and are never built here: every cargo
# invocation names -p lemmate-server -p lemmate-cli explicitly. Their manifests still have to be
# present and stubbed below, because cargo will not load a workspace with a member missing.

# ---- Stage 1: build the web client (ui/dist) -------------------------------------------------
FROM node:24-alpine AS ui
WORKDIR /ui

# Manifests first so `npm ci` is cached until the lockfile actually changes.
COPY ui/package.json ui/package-lock.json ./
RUN npm ci

# Sources second. ui/node_modules and ui/dist are excluded by .dockerignore, so this neither
# clobbers the installed tree nor imports a stale local build.
COPY ui/ ./
RUN npm run build


# ---- Stage 2: build the Rust binaries --------------------------------------------------------
# rust:1-bookworm is buildpack-deps based, so it already ships gcc + libc headers, which the
# bundled SQLite (rusqlite "bundled") compiles against. TLS is rustls/ring — pure Rust and asm,
# no OpenSSL headers needed.
FROM rust:1-bookworm AS build
WORKDIR /src

# Never inherit a host target-dir: .cargo/config.toml redirects it to a machine-local path and
# is excluded from the build context, but an explicit env var is precedence-safe regardless.
ENV CARGO_TARGET_DIR=/src/target

# --- dependency cache layer ---
# Copy only the manifests + lockfile and build throwaway crate roots, so the (large) dependency
# graph is compiled into a layer that is reused until Cargo.toml/Cargo.lock change. This works
# on plain `docker build` with no BuildKit cache mounts required.
#
# Every member's manifest, including the two shells that are never compiled here.
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/cli/Cargo.toml crates/cli/Cargo.toml
COPY crates/desktop/Cargo.toml crates/desktop/Cargo.toml
COPY crates/mobile/Cargo.toml crates/mobile/Cargo.toml
RUN mkdir -p crates/core/src crates/server/src crates/cli/src crates/desktop/src crates/mobile/src \
    && echo '' > crates/core/src/lib.rs \
    && echo 'fn main() {}' > crates/server/src/main.rs \
    && echo 'fn main() {}' > crates/cli/src/main.rs \
    && echo '' > crates/cli/src/lib.rs \
    && echo '' > crates/server/src/lib.rs \
    && echo 'fn main() {}' > crates/desktop/src/main.rs \
    && echo 'fn main() {}' > crates/desktop/build.rs \
    && echo '' > crates/mobile/src/lib.rs \
    && echo 'fn main() {}' > crates/mobile/src/main.rs \
    && echo 'fn main() {}' > crates/mobile/build.rs \
    && cargo build --release --locked -p lemmate-server -p lemmate-cli

# --- real build ---
# COPY preserves the context's mtimes, which are older than the stub artifacts just produced, so
# cargo would consider the stubs fresh. Drop exactly the three workspace packages' artifacts
# (dependencies stay cached) and rebuild for real.
COPY crates/ crates/
RUN cargo clean --release -p lemmate-core -p lemmate-server -p lemmate-cli \
    && cargo build --release --locked -p lemmate-server -p lemmate-cli


# ---- Stage 3: runtime ------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates: outbound https:// (e.g. a CLI run inside the container).
# curl: only used by HEALTHCHECK below.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Fixed uid so a host bind mount can be chowned deterministically: chown -R 10001:10001 ./data
RUN groupadd --gid 10001 lemmate \
    && useradd --uid 10001 --gid 10001 --home-dir /app --no-create-home --shell /usr/sbin/nologin lemmate

WORKDIR /app
COPY --from=build /src/target/release/lemmate-server /usr/local/bin/lemmate-server
COPY --from=build /src/target/release/lemmate        /usr/local/bin/lemmate
COPY --from=ui    /ui/dist                         /app/web

# lemmate.db + attachments/ live here. Created (and owned) before VOLUME so a fresh named volume
# inherits the ownership. Bind mounts and fly.io volumes are root-owned and need a manual chown
# — see docs/deploy.md.
RUN mkdir -p /data && chown -R 10001:10001 /data
VOLUME ["/data"]

USER 10001:10001
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["lemmate-server"]
CMD ["--bind", "0.0.0.0:8080", "--data-dir", "/data", "--web-dir", "/app/web"]
