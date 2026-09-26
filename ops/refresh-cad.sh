#!/usr/bin/env bash
# Downloads fresh CAD parcels and county address points (about 25 minutes, one request
# every 5 s). The running app picks up the new file without a restart; the old file is
# kept if the download fails. Run nightly from cron:
#   30 2 * * *  /opt/county-router/ops/refresh-cad.sh >> /var/log/county-router-cad.log 2>&1
set -euo pipefail
cd "$(dirname "$0")/.."
docker compose run --rm --user "$(id -u):$(id -g)" cad-refresh
