# norn-cell-rivet, the Rivet runner that hosts Norn cells (D131).
# Build from the repository root:
#   docker build -f apps/norn-cell-rivet/Dockerfile -t norn-cell-rivet .
# It joins the engine named by RIVET_ENDPOINT and serves the router on
# NORN_LISTEN (8787 here); see apps/norn-cell-rivet/README.md.
FROM rust:1.98.0-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS build
WORKDIR /src
COPY . .
# Its own workspace and lockfile (crates/norn-cell-rivet/Cargo.toml says why).
RUN cd crates/norn-cell-rivet && cargo build --locked --release --bin norn-cell-rivet

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /src/crates/norn-cell-rivet/target/release/norn-cell-rivet /usr/local/bin/norn-cell-rivet
ENV NORN_LISTEN=0.0.0.0:8787
EXPOSE 8787
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/norn-cell-rivet"]
