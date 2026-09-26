# Shared by the ops scripts: run from the repo root, read .env, one job at a time, and
# optional pings to a monitoring service (e.g. healthchecks.io) on success or failure.
cd "$(dirname "${BASH_SOURCE[0]}")/.."
set -a
# shellcheck disable=SC1091
[[ -f .env ]] && . ./.env
set +a
log() { printf '%s %s\n' "$(date '+%F %T')" "$*"; }

# One refresh job at a time (the CAD download can still be running when the weekly
# rebuild starts). The watchdog also skips its checks while a job holds the lock.
LOCK_FILE=/tmp/county-router-ops.lock
take_lock() {
  exec 9>"$LOCK_FILE"
  flock -w "${1:-7200}" 9 || { log "another job still holds $LOCK_FILE"; exit 1; }
}

# ping_on_exit URL: on exit, GET URL on success or URL/fail on failure; no URL, no ping.
ping_on_exit() {
  PING_URL=${1:-}
  trap 'status=$?; if [[ -n $PING_URL ]]; then
          if [[ $status == 0 ]]; then suffix=""; else suffix=/fail; fi
          curl -fsS -m 10 --retry 3 -o /dev/null "$PING_URL$suffix" || true
        fi' EXIT
}
