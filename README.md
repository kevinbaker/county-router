# county-router

Route planner for addresses in Collin County, TX: geocode a list of stops, find the
fastest order, and draw the route. Runs on OpenStreetMap data with self-hosted OSRM,
VROOM and Nominatim.

## Layout

| Path | What |
| --- | --- |
| `crates/core` | Shared types and the county boundary check |
| `crates/api` | axum HTTP API: `/api/geocode`, `/api/solve`, `/api/boundary`, `/api/health` |
| `crates/dataprep` | Offline data tools (`boundary` today; address points and POIs later) |
| `data/build.sh` | Downloads, clips and builds the routing data into `data/out/` |
| `data/boundary/` | Collin County outline (FIPS 48085), plain and buffered 5 km, committed |
| `compose.dev.yaml` | OSRM, VROOM and Nominatim for local development |
| `conf/vroom/config.yml` | vroom-express settings, pointed at the `osrm` container |
| `web/` | Prototype UI: Vite + TypeScript + MapLibre |
| `Dockerfile`, `compose.yaml`, `Caddyfile` | Production image and stack; see [DEPLOY.md](DEPLOY.md) |
| `ops/` | Nightly CAD and weekly map refresh scripts for the server |

## Setup

Needs Docker (with your user in the `docker` group), Rust, Node 20+, `curl` and `osmium-tool`.

```sh
sudo usermod -aG docker $USER      # then log out and back in
sudo apt install osmium-tool
cp .env.example .env
```

## Build the data

```sh
data/build.sh            # Texas download (~720 MB, cached), clip, OSRM, map tiles
data/build.sh --cad      # also CAD parcels + county address points (~25 min, gentle)
```

The CAD download alone: `cargo run --release -p county-dataprep -- cad`. It writes
`data/out/cad.sqlite` and replaces it only when the download is complete; the API
picks up a new copy without a restart.

The boundary files are rebuilt only when `data/boundary/collin.geojson` is missing.

## Run

```sh
docker compose -f compose.dev.yaml up -d   # first start imports Nominatim; watch with `logs -f nominatim`
(cd web && npm install && npm run build)
cargo run -p county-api                    # UI and API on http://127.0.0.1:8000
cargo test
```

For UI work, run `npm run dev` in `web/` instead of building: Vite serves the UI on
http://127.0.0.1:5173 with live reload and proxies `/api` to the Rust API on :8000.

Stops are CAD property IDs or addresses. Each is matched to a Collin CAD parcel (by ID,
or by the parcel's site address), then placed on the county 911 point for the building
when there is one, otherwise on the lot edge facing the addressed street. Routing
targets that street, so long rural lots aren't reached from their back road. Parcels
and building points come from the local `cad.sqlite` when present, with the live
public ArcGIS layers as a fallback for anything newer.
Addresses with no parcel fall back to the Census Geocoder, then Nominatim (street
level only in most of the county). The basemap is self-hosted: vector tiles for the region extracted from
Protomaps' daily build (`data/out/region.pmtiles`), with fonts and icons in
`web/public/basemap/`.

## Deploy

See [DEPLOY.md](DEPLOY.md): one server with Docker Compose, Caddy for HTTPS (no login),
and cron jobs for the CAD and map refreshes.
