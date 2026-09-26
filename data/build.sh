#!/usr/bin/env bash
# Builds the routing and geocoding data for Collin County and its neighbours, so stops
# (and starts) just outside the county can be routed too.
#
#   data/build.sh            boundary (if missing), Texas download, clip, OSRM
#   data/build.sh --tiles    also build the PMTiles basemap with Planetiler
#   data/build.sh --cad      also download CAD parcels and county address points
#                            (about 25 min: one request every 5 s, to go easy on
#                            the county's servers; CAD updates daily)
#
# OUT (default data/out) lets the weekly refresh build into a fresh directory.
# Nominatim imports data/out/collin.osm.pbf itself on first container start. The files
# keep "collin" in their names but cover the whole region.
set -euo pipefail
cd "$(dirname "$0")/.."

OSRM_IMAGE=${OSRM_IMAGE:-ghcr.io/project-osrm/osrm-backend:v26.9.0-debian}
PLANETILER_IMAGE=${PLANETILER_IMAGE:-ghcr.io/onthegomap/planetiler:latest}
TEXAS_URL=https://download.geofabrik.de/north-america/us/texas-latest.osm.pbf
# County outlines from Census TIGERweb.
TIGERWEB=https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/State_County/MapServer/1/query
# Collin County, TX (FIPS 48085).
COLLIN="'48085'"
# The routable region: Collin plus Dallas, Denton, Rockwall, Hunt, Fannin and Grayson.
REGION="'48085','48113','48121','48397','48231','48147','48181'"
BUFFER_KM=5
CACHE=data/cache
OUT=${OUT:-data/out}

tiles=false
cad=false
for arg in "$@"; do
  case $arg in
    --tiles) tiles=true ;;
    --cad) cad=true ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
done

need() { command -v "$1" >/dev/null || { echo "missing $1: $2" >&2; exit 1; }; }
need curl "sudo apt install curl"
need osmium "sudo apt install osmium-tool"
need docker "sudo apt install docker.io"
docker info >/dev/null 2>&1 || { echo "cannot reach Docker; add yourself to the docker group" >&2; exit 1; }

mkdir -p "$CACHE" "$OUT" data/boundary
log() { printf '\n== %s\n' "$*"; }

county_outline() { # FIPS list, output name, where the buffered outline goes
  curl -fsS -G "$TIGERWEB" --data-urlencode "where=GEOID IN ($1)" \
    -d outFields=GEOID,NAME -d outSR=4326 -d f=geojson -o "$CACHE/$2-tigerweb.geojson"
  cargo run -q --release -p county-dataprep -- boundary \
    --input "$CACHE/$2-tigerweb.geojson" --buffer-km "$BUFFER_KM" \
    --out "data/boundary/$2.geojson" --out-buffered "$3"
}

# Collin's outline is drawn on the map and flags stops outside the county; the region
# decides what can be routed, and its buffered outline cuts the OSM extract.
for name in collin region; do
  if [[ ! -f data/boundary/$name.geojson ]]; then
    log "Boundary: $name"
    need cargo "https://rustup.rs"
    if [[ $name == collin ]]; then
      county_outline "$COLLIN" collin "$CACHE/collin-buffered.geojson"
    else
      county_outline "$REGION" region data/boundary/region-buffered.geojson
    fi
  fi
done

log "Texas extract (skipped if unchanged)"
curl -fL --progress-bar -z "$CACHE/texas.osm.pbf" -o "$CACHE/texas.osm.pbf" "$TEXAS_URL"

log "Clip to the region + ${BUFFER_KM} km"
osmium extract --overwrite --strategy smart \
  --polygon data/boundary/region-buffered.geojson \
  -o "$CACHE/collin-clip.osm.pbf" "$CACHE/texas.osm.pbf"
# The clip drops the Texas state boundary (none of its ways cross the region).
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
if $cad; then
  log "CAD parcels and county address points"
  need cargo "https://rustup.rs (or use ops/refresh-cad.sh, which runs it in Docker)"
  cargo run -q --release -p county-dataprep -- cad --out "$OUT/cad.sqlite" --delay-ms 5000
fi

osm_ts=$(osmium fileinfo -g header.option.osmosis_replication_timestamp "$CACHE/texas.osm.pbf" || true)
printf 'built %s from OSM data as of %s\n' "$(date -u +%FT%TZ)" "${osm_ts:-unknown}" > "$OUT/BUILD_INFO"
log "Done: $(cat "$OUT/BUILD_INFO")"
