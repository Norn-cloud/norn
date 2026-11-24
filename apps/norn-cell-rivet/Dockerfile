# norn-cell-rivet, the Rivet runner that hosts Norn cells (D131).
# Build from the repository root:
#   docker build -f apps/norn-cell-rivet/Dockerfile -t norn-cell-rivet .
# It joins the engine named by RIVET_ENDPOINT and serves the router on
# the platform's PORT (8787 by default); see apps/norn-cell-rivet/README.md.
# The toolchain CI builds the workspace with (.github/workflows/ci-policy.yml): an older one does not
# compile the shared crates (norn-kernel iterates `&Box<[FactKey; 2]>`, which Rust 1.98 refuses).
FROM rust:1.100.0-bookworm@sha256:1b9dc70890ec494e466c87f5ad41ad544bfb30ea6acc99a3e068c47a4ae3f23e AS build
WORKDIR /src
COPY . .
# Its own workspace and lockfile (crates/norn-cell-rivet/Cargo.toml says why).
RUN cd crates/norn-cell-rivet && cargo build --locked --release --bin norn-cell-rivet

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /src/crates/norn-cell-rivet/target/release/norn-cell-rivet /usr/local/bin/norn-cell-rivet
ENV PORT=8787
EXPOSE 8787
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/norn-cell-rivet"]
