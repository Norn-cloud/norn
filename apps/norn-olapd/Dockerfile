# norn-olapd, the tenant analytics engine next to the cell host (D102).
# Build from the repository root:
#   docker build -f apps/norn-olapd/Dockerfile -t norn-olapd .
# Run with NORN_SECRET_DUCKDB_TOKEN set and a volume at /data; it listens on
# 8790 and answers only the cell (see crates/norn-cell-do/src/olapd.rs).
FROM rust:1.98.0-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p norn-cell-do --features duckdb --bin norn-olapd \
 && install -d -o 65532 -g 65532 /data

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/norn-olapd /usr/local/bin/norn-olapd
COPY --from=build --chown=65532:65532 /data /data
ENV NORN_OLAP_DATA=/data NORN_OLAP_ADDR=0.0.0.0:8790
VOLUME /data
EXPOSE 8790
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=5s CMD ["/usr/local/bin/norn-olapd", "--health"]
ENTRYPOINT ["/usr/local/bin/norn-olapd"]
