# syntax=docker/dockerfile:1.10
# Norn, self-hosted: celld serving the tenant cell Worker and Studio from one image (#728).
# Build context: the repository root (compose.yaml sets it). Every tool is pinned.

# --- The commit this image is built from: Studio's STUDIO_BUILD ------------------------------
# A clone carries it in .git; otherwise pass it: `NORN_COMMIT=<40 hex> docker compose build`.
# Only this stage reads .git, so a new commit does not rebuild the others.
FROM rust:1.98.0-bookworm AS commit
ARG NORN_COMMIT=""
RUN --mount=type=bind,target=/ctx \
    commit="${NORN_COMMIT:-$(git -C /ctx -c safe.directory=/ctx rev-parse HEAD 2>/dev/null || true)}"; \
    echo "$commit" | grep -Eqx '[0-9a-f]{40}' \
      || { echo "error: no commit; build from a git clone or set NORN_COMMIT" >&2; exit 1; }; \
    mkdir -p /out && echo "$commit" > /out/commit

# --- The cell Worker and the checker, compiled to WebAssembly -------------------------------
FROM rust:1.98.0-bookworm AS wasm
RUN rustup target add wasm32-unknown-unknown \
 && cargo install --locked wasm-bindgen-cli --version 0.2.129
WORKDIR /src
COPY --exclude=apps/self-host --exclude=.git . .
ENV CARGO_TARGET_DIR=/target
# A deployable cell: no `fixtures` feature, so only a verified host token is a caller.
RUN cargo build --locked --release -p norn-cli \
 && cp /target/release/norn /out-norn \
 && cargo build --locked -p norn-cell-do --target wasm32-unknown-unknown --profile wasm-release \
 && wasm-bindgen /target/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm \
      --target web --out-name norn_cell_do --out-dir /out/cell/pkg \
 && cp apps/norn-cell-do/worker.mjs /out/cell/ \
 && crates/norn-checker-wasm/scripts/build-pkg.sh /out/checker
# --- Studio: the bundle and the Worker sources celld deploys ----------------------------------
FROM oven/bun:1.3.14-debian AS studio
WORKDIR /src
COPY --exclude=apps/self-host --exclude=.git . .
COPY --from=wasm /out/checker apps/studio/src/checker
RUN cd apps/studio \
 && bun install --frozen-lockfile --ignore-scripts --backend copyfile \
 && NODE_ENV=production bun run build

# --- Runtime -------------------------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
# celld v0.6.3 (tag commit 8702f6d186c77698dbfb30cf29e3b1922a88f008), checked by its published digest.
ARG CELLD_SHA256=5084a083472cf036551a7eac63fa24d22bb9a56323c517f63205724c12783047
RUN curl -fsSL -o /tmp/celld.gz https://github.com/denoland/celld/releases/download/v0.6.3/celld-x86_64-unknown-linux-gnu.gz \
 && echo "${CELLD_SHA256}  /tmp/celld.gz" | sha256sum -c - \
 && gunzip -c /tmp/celld.gz > /usr/local/bin/celld && chmod 0755 /usr/local/bin/celld && rm /tmp/celld.gz
COPY --from=studio /usr/local/bin/bun /usr/local/bin/bun
COPY --from=studio /src/apps/studio /opt/norn/studio
COPY --from=wasm /out/cell /opt/norn/cell
COPY --from=commit /out/commit /opt/norn/commit
COPY --from=wasm /out-norn /usr/local/bin/norn
COPY apps/self-host/start.sh apps/self-host/setup.ts /opt/norn/
RUN useradd --system --uid 10001 --home-dir /data norn && mkdir -p /data && chown norn /data /opt/norn/cell /opt/norn/studio
USER norn
ENV CELLD_ESBUILD=/opt/norn/studio/node_modules/.bin/esbuild CELLD_WATCH=/data/celld
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/bin/sh", "/opt/norn/start.sh"]
