# Production image: the API (serving the built UI) plus the county-dataprep tool.

FROM node:24-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS rust
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p county-api -p county-dataprep \
    && cp target/release/county-api target/release/county-dataprep /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 app
COPY --from=rust /usr/local/bin/county-api /usr/local/bin/county-dataprep /usr/local/bin/
COPY --from=web /src/web/dist /app/web/dist
COPY data/boundary /app/data/boundary
WORKDIR /app
ENV BIND=0.0.0.0:8000 \
    WEB_DIR=/app/web/dist \
    BOUNDARY_PATH=/app/data/boundary/collin.geojson \
    REGION_PATH=/app/data/boundary/region.geojson \
    CAD_DB=/data/cad.sqlite \
    TILES_PATH=/data/region.pmtiles \
    BUILD_INFO_PATH=/data/BUILD_INFO \
    RUST_LOG=info
USER app
EXPOSE 8000
CMD ["county-api"]
