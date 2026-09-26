# county-router

Route planner for addresses in Collin County, TX: geocode a list of stops, find the
fastest order, and draw the route. Runs on OpenStreetMap data with self-hosted OSRM,
VROOM and Nominatim.

## Layout

| Path | What |
| --- | --- |
| `crates/core` | Shared types and the county boundary check |
| `crates/api` | axum HTTP API |
| `crates/dataprep` | Offline data tools (`boundary` today; address points and POIs later) |
| `data/build.sh` | Downloads, clips and builds the routing data into `data/out/` |
| `data/boundary/` | Collin County outline (FIPS 48085), plain and buffered 5 km, committed |
| `compose.dev.yaml` | OSRM, VROOM and Nominatim for local development |
| `conf/vroom/config.yml` | vroom-express settings, pointed at the `osrm` container |

## Setup

Needs Docker (with your user in the `docker` group), Rust, `curl` and `osmium-tool`.

```sh
sudo usermod -aG docker $USER      # then log out and back in
sudo apt install osmium-tool
cp .env.example .env
```

## Build the data

```sh
data/build.sh            # Texas download (~720 MB, cached), clip, OSRM build
data/build.sh --tiles    # also the PMTiles basemap
```

The boundary files are rebuilt only when `data/boundary/collin.geojson` is missing.

## Run

```sh
docker compose -f compose.dev.yaml up -d   # first start imports Nominatim; watch with `logs -f nominatim`
cargo run -p county-api                    # http://127.0.0.1:8000/health
cargo test
```
