# Deploying

One Linux server runs everything with Docker Compose: the app (API and UI), OSRM
routing, VROOM stop ordering, Nominatim street geocoding, and Caddy for HTTPS and a
shared login. Only ports 80 and 443 are exposed.

## Server

| | Minimum | Measured in a full-stack test |
| --- | --- | --- |
| RAM | 8 GB | about 1.3 GB in use (Nominatim 700 MB, OSRM 550 MB, VROOM 40 MB, Caddy 18 MB, API 9 MB); Nominatim's import peaks higher |
| CPU | 2 vCPU | a 250-stop matrix takes 0.4 s |
| Disk | 30 GB | Texas download 720 MB, built data about 1.1 GB, CAD copy 200 MB, images about 2 GB |

The road data covers Collin County and its six neighbours (Dallas, Denton, Rockwall,
Hunt, Fannin, Grayson), so stops just outside the county can be routed. Any Debian or Ubuntu server with Docker works.

Outbound access is needed to: download.geofabrik.de (weekly map data),
services1/services2.arcgis.com (CAD and county data), geocoding.geo.census.gov
(fallback geocoding for addresses with no CAD parcel), and tile.openstreetmap.org
(map background, loaded by the users' browsers).

## First deploy

```sh
# On the server, as a user in the docker group
sudo apt install docker.io docker-compose osmium-tool git
git clone <repo> /opt/county-router && cd /opt/county-router

cp .env.production.example .env
docker run --rm caddy:2 caddy hash-password --plaintext 'choose-a-password'
nano .env      # DOMAIN, AUTH_USER, AUTH_HASH (keep the single quotes)

data/build.sh                                   # map data, about 5–10 min
docker compose build
docker compose run --rm --user "$(id -u):$(id -g)" cad-refresh   # CAD data, about 25 min
docker compose up -d
```

Point DNS for both the domain and `www` at the server before starting; Caddy gets
certificates on first start. Plain `http://` redirects to HTTPS (308) and `www` to
`https://<domain>` (301). With Cloudflare, use "DNS only" until the first certificate is
issued, then proxy with SSL mode "Full (strict)" if wanted. Nominatim imports for 5–15 minutes after the first
start; CAD lookups and routing work before it finishes.

Instead of building data on the server, you can copy `data/out/` from a machine that
already has it (`rsync -a data/out/ server:/opt/county-router/data/out/`).

## Updating the app

```sh
git pull
docker compose up -d --build
```

## Keeping data fresh

CAD updates its parcel layer daily; OpenStreetMap roads change more slowly.

```cron
30 2 * * *  /opt/county-router/ops/refresh-cad.sh >> /opt/county-router/logs/refresh-cad.log 2>&1
0 3 * * 0   /opt/county-router/ops/refresh-osm.sh >> /opt/county-router/logs/refresh-osm.log 2>&1
```

(`mkdir -p /opt/county-router/logs` first; install `cron` if the server lacks it.)

- `refresh-cad.sh` downloads parcels and address points gently (one request every 5 s).
  The app switches to the new file without a restart; a failed download keeps the old one.
- `refresh-osm.sh` rebuilds the map data, restarts OSRM (a few seconds) and re-imports
  Nominatim (5–15 minutes of street-level geocoding downtime).

Nothing else needs backing up: all data is rebuilt from public sources.

## Checks

```sh
docker compose ps                       # all services up; api and vroom healthy
curl -u user:pass https://<host>/api/health   # ok: true, with the data build date
```

## Before wider use

- **Map background.** The UI loads tiles from tile.openstreetmap.org, whose usage policy
  allows light use only. Fine for a few users; replace with a self-hosted PMTiles
  basemap (`data/build.sh --tiles`) before rolling out widely.
- **Login.** One shared login via Caddy. Per-user accounts are not built.
- **Privacy.** Addresses with no CAD parcel are sent to the Census Geocoder. Owner names
  are never fetched or stored.
