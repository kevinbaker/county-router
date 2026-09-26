#!/usr/bin/env bash
# Builds the routing and geocoding data for Collin County.
#
#   data/build.sh            boundary (if missing), Texas download, clip, OSRM
#   data/build.sh --tiles    also build the PMTiles basemap with Planetiler
#
# OUT (default data/out) lets the weekly refresh build into a fresh directory.
# Nominatim imports data/out/collin.osm.pbf itself on first container start.
set -euo pipefail
cd "$(dirname "$0")/.."

OSRM_IMAGE=${OSRM_IMAGE:-ghcr.io/project-osrm/osrm-backend:v26.9.0-debian}
PLANETILER_IMAGE=${PLANETILER_IMAGE:-ghcr.io/onthegomap/planetiler:latest}
TEXAS_URL=https://download.geofabrik.de/north-america/us/texas-latest.osm.pbf
# Collin County, TX (FIPS 48085) from Census TIGERweb.
COUNTY_URL="https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/State_County/MapServer/1/query?where=GEOID%3D%2748085%27&outFields=GEOID,NAME&outSR=4326&f=geojson"
BUFFER_KM=5
CACHE=data/cache
OUT=${OUT:-data/out}

tiles=false
[[ ${1:-} == --tiles ]] && tiles=true

need() { command -v "$1" >/dev/null || { echo "missing $1: $2" >&2; exit 1; }; }
need curl "sudo apt install curl"
need cargo "https://rustup.rs"
need osmium "sudo apt install osmium-tool"
need docker "sudo apt install docker.io"
docker info >/dev/null 2>&1 || { echo "cannot reach Docker; add yourself to the docker group" >&2; exit 1; }

mkdir -p "$CACHE" "$OUT" data/boundary
log() { printf '\n== %s\n' "$*"; }

if [[ ! -f data/boundary/collin.geojson ]]; then
  log "County boundary"
  curl -fsS "$COUNTY_URL" -o "$CACHE/collin-tigerweb.geojson"
  cargo run -q --release -p county-dataprep -- boundary \
    --input "$CACHE/collin-tigerweb.geojson" --buffer-km "$BUFFER_KM" \
    --out data/boundary/collin.geojson \
    --out-buffered data/boundary/collin-buffered.geojson
fi

log "Texas extract (skipped if unchanged)"
curl -fL --progress-bar -z "$CACHE/texas.osm.pbf" -o "$CACHE/texas.osm.pbf" "$TEXAS_URL"

log "Clip to Collin County + ${BUFFER_KM} km"
osmium extract --overwrite --strategy smart \
  --polygon data/boundary/collin-buffered.geojson \
  -o "$CACHE/collin-clip.osm.pbf" "$CACHE/texas.osm.pbf"
# The clip drops the Texas state boundary (none of its ways cross the county).
# Without it Nominatim doesn't know addresses are in TX, and every "…, TX" query fails.
osmium tags-filter --overwrite -o "$CACHE/texas-state.osm.pbf" "$CACHE/texas.osm.pbf" r/ISO3166-2=US-TX
osmium merge --overwrite -o "$OUT/collin.osm.pbf" "$CACHE/collin-clip.osm.pbf" "$CACHE/texas-state.osm.pbf"

log "OSRM (car, MLD)"
osrm() { docker run --rm -u "$(id -u):$(id -g)" -v "$PWD/$OUT:/data" "$OSRM_IMAGE" "$@"; }
osrm osrm-extract -p /opt/car.lua /data/collin.osm.pbf
osrm osrm-partition /data/collin.osrm
osrm osrm-customize /data/collin.osrm

if $tiles; then
  log "PMTiles basemap"
  mkdir -p "$CACHE/planetiler"
  docker run --rm -u "$(id -u):$(id -g)" \
    -v "$PWD/$OUT:/data" -v "$PWD/$CACHE/planetiler:/data/sources" \
    "$PLANETILER_IMAGE" --osm-path=/data/collin.osm.pbf \
    --output=/data/collin.pmtiles --download --force
fi

# The clipped file drops the replication header, so read it from the source extract.
osm_ts=$(osmium fileinfo -g header.option.osmosis_replication_timestamp "$CACHE/texas.osm.pbf" || true)
printf 'built %s from OSM data as of %s\n' "$(date -u +%FT%TZ)" "${osm_ts:-unknown}" > "$OUT/BUILD_INFO"
log "Done: $(cat "$OUT/BUILD_INFO")"
