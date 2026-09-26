#!/usr/bin/env bash
# Every 5 minutes: starts any service that has stopped and restarts any that Docker
# reports unhealthy. Stays out of the way while a refresh job is running.
set -uo pipefail
source "$(dirname "$0")/common.sh"
exec 9>"$LOCK_FILE"
flock -n 9 || exit 0
before=$(docker compose ps --status running --services | sort)
docker compose up -d --no-recreate >/dev/null 2>&1
after=$(docker compose ps --status running --services | sort)
[[ $before != "$after" ]] && log "started: $(comm -13 <(echo "$before") <(echo "$after") | tr '\n' ' ')"
for svc in $(docker compose ps --format '{{.Service}} {{.Health}}' | awk '$2 == "unhealthy" {print $1}'); do
  log "restarting unhealthy $svc"
  docker compose restart "$svc" >/dev/null
done
if [[ -n ${WATCHDOG_PING_URL:-} ]]; then curl -fsS -m 10 -o /dev/null "$WATCHDOG_PING_URL" || true; fi
