# norn-olapd, the tenant analytics engine next to the cell host (D102).
# Build from the repository root:
#   docker build -f apps/norn-olapd/Dockerfile -t norn-olapd .
# Run with NORN_OLAP_KEYS_FILE naming a mounted keyring of the cells' public
# keys and a volume at /data; it listens on 8790 and answers each cell for its
# own tenant only (OLAP-CRED-1, see crates/norn-cell-do/src/olapd.rs).
FROM rust:1.100.0-bookworm@sha256:1b9dc70890ec494e466c87f5ad41ad544bfb30ea6acc99a3e068c47a4ae3f23e AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p norn-cell-do --features duckdb --bin norn-olapd \
 && install -d -m 0700 -o 65532 -g 65532 /data

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /src/target/release/norn-olapd /usr/local/bin/norn-olapd
COPY --from=build --chown=65532:65532 /data /data
ENV NORN_OLAP_DATA=/data NORN_OLAP_ADDR=0.0.0.0:8790
VOLUME /data
EXPOSE 8790
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=5s CMD ["/usr/local/bin/norn-olapd", "--health"]
ENTRYPOINT ["/usr/local/bin/norn-olapd"]
