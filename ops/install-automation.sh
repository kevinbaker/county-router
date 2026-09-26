#!/usr/bin/env bash
# One-time setup so the server runs unattended; safe to re-run. Needs sudo.
#   - cron: nightly CAD download, weekly map rebuild, watchdog every 5 minutes
#   - logrotate for logs/*.log
#   - Debian security updates, with a reboot at 04:30 when one needs it
#     (the VM, Docker and every service come back on their own)
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
mkdir -p logs

command -v crontab >/dev/null || sudo apt-get install -y -qq cron
sudo systemctl enable --now cron >/dev/null
( crontab -l 2>/dev/null | grep -v 'county-router' || true
  echo "# county-router (times are the server's local time)"
  echo "30 2 * * * $root/ops/refresh-cad.sh >> $root/logs/refresh-cad.log 2>&1 # county-router"
  echo "0 3 * * 0 $root/ops/refresh-osm.sh >> $root/logs/refresh-osm.log 2>&1 # county-router"
  echo "*/5 * * * * $root/ops/watchdog.sh >> $root/logs/watchdog.log 2>&1 # county-router"
) | crontab -

sudo tee /etc/logrotate.d/county-router >/dev/null <<ROTATE
$root/logs/*.log {
    weekly
    rotate 8
    compress
    missingok
    notifempty
    copytruncate
}
ROTATE

sudo apt-get install -y -qq unattended-upgrades >/dev/null
sudo tee /etc/apt/apt.conf.d/20auto-upgrades >/dev/null <<'APT'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT
sudo tee /etc/apt/apt.conf.d/52county-router >/dev/null <<'APT'
Unattended-Upgrade::Automatic-Reboot "true";
Unattended-Upgrade::Automatic-Reboot-Time "04:30";
APT

echo "installed:"; crontab -l | grep county-router
