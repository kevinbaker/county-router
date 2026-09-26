#!/usr/bin/env bash
# Nightly: downloads fresh CAD parcels and county address points (about 25 minutes, one
# request every 5 s). The app picks up the new file without a restart; a failed
# download keeps the old one. Installed by ops/install-automation.sh.
set -euo pipefail
source "$(dirname "$0")/common.sh"
ping_on_exit "${CAD_PING_URL:-}"
take_lock
log "CAD refresh starting"
docker compose run --rm --user "$(id -u):$(id -g)" cad-refresh
log "CAD refresh done"
