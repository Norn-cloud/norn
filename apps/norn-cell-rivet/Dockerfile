# norn-cell-rivet, the Rivet runner that hosts Norn cells (D131).
# Build from the repository root:
#   docker build -f apps/norn-cell-rivet/Dockerfile -t norn-cell-rivet .
# It joins the engine named by RIVET_ENDPOINT and serves the router on
# NORN_LISTEN (8787 here); see apps/norn-cell-rivet/README.md.
FROM rust:1.98.0-bookworm AS build
WORKDIR /src
COPY . .
# Its own workspace and lockfile (crates/norn-cell-rivet/Cargo.toml says why).
RUN cd crates/norn-cell-rivet && cargo build --locked --release --bin norn-cell-rivet

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/crates/norn-cell-rivet/target/release/norn-cell-rivet /usr/local/bin/norn-cell-rivet
ENV NORN_LISTEN=0.0.0.0:8787
EXPOSE 8787
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/norn-cell-rivet"]
