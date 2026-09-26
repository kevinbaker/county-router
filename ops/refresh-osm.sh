#!/usr/bin/env bash
# Weekly: rebuilds the OpenStreetMap data (roads, street geocoding, map tiles) into a
# fresh directory and swaps it in only if every file was built; the previous week's
# data stays in data/out.prev for rollback. Then reloads OSRM and the app and
# re-imports Nominatim (street-only lookups are unavailable for about 35 minutes;
# CAD, Census and routing keep working). Installed by ops/install-automation.sh.
#
# Roll back: docker compose stop osrm api && mv data/out data/out.bad &&
#            mv data/out.prev data/out && docker compose up -d --force-recreate osrm api
set -euo pipefail
source "$(dirname "$0")/common.sh"
ping_on_exit "${OSM_PING_URL:-}"
take_lock

next=data/out.next
rm -rf "$next"
mkdir -p "$next"
log "building into $next"
OUT=$next data/build.sh

# The CAD copy is refreshed separately; carry the current one over (a hard link, no copy).
[[ -f data/out/cad.sqlite ]] && ln -f data/out/cad.sqlite "$next/cad.sqlite"
for f in collin.osm.pbf collin.osrm.mldgr collin.osrm.partition region.pmtiles BUILD_INFO; do
  [[ -s $next/$f ]] || { log "missing $next/$f; keeping the current data"; exit 1; }
done

log "swapping in the new data"
rm -rf data/out.prev
mv data/out data/out.prev
mv "$next" data/out
# Recreate so the containers mount the new directory.
docker compose up -d --force-recreate osrm api
docker compose rm -sf nominatim
docker volume rm county-router_nominatim-db
docker compose up -d nominatim
docker image prune -f >/dev/null
log "OSM refresh done: $(cat data/out/BUILD_INFO)"
