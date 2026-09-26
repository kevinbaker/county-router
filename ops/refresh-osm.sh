#!/usr/bin/env bash
# Rebuilds the OpenStreetMap data (routing and street geocoding), then reloads OSRM and
# re-imports Nominatim. Routing is down for a few seconds and street-level geocoding for
# 5–15 minutes; CAD and Census lookups keep working. Run weekly from cron:
#   0 3 * * 0  /opt/county-router/ops/refresh-osm.sh >> /opt/county-router/logs/refresh-osm.log 2>&1
set -euo pipefail
cd "$(dirname "$0")/.."
data/build.sh
docker compose restart osrm
docker compose rm -sf nominatim
docker volume rm county-router_nominatim-db
docker compose up -d nominatim
