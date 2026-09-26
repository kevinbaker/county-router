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

## Setup

Needs Docker (with your user in the `docker` group), Rust, Node 20+, `curl` and `osmium-tool`.

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
(cd web && npm install && npm run build)
cargo run -p county-api                    # UI and API on http://127.0.0.1:8000
cargo test
```

For UI work, run `npm run dev` in `web/` instead of building: Vite serves the UI on
http://127.0.0.1:5173 with live reload and proxies `/api` to the Rust API on :8000.

Stops are CAD property IDs or addresses. Each is matched to a Collin CAD parcel (by ID,
or by the parcel's site address), then placed on the county 911 point for the building
when there is one, otherwise on the lot edge facing the addressed street. Routing
targets that street, so long rural lots aren't reached from their back road. Both
CAD and county services are public ArcGIS layers, queried live and cached in memory.
Addresses with no parcel fall back to the Census Geocoder, then Nominatim (street
level only in most of the county). The prototype basemap uses openstreetmap.org tiles, which is fine for
light development use only; replace it with the PMTiles build before real use.
