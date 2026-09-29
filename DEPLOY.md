# Deploying

One Linux server runs everything with Docker Compose: the app (API and UI), OSRM
routing, VROOM stop ordering, Nominatim street geocoding, and Caddy for HTTPS.
There is no login: the site is public. Only ports 80 and 443 are exposed.

## Server

| | Minimum | Measured in a full-stack test |
| --- | --- | --- |
| RAM | 8 GB | about 1.3 GB in use (Nominatim 700 MB, OSRM 550 MB, VROOM 40 MB, Caddy 18 MB, API 9 MB); Nominatim's import peaks higher |
| CPU | 2 vCPU | a 250-stop matrix takes 0.4 s |
| Disk | 30 GB | Texas download 720 MB, built data about 1.1 GB, CAD copy 200 MB, images about 2 GB |

The road data covers Collin County, its six neighbours (Dallas, Denton, Rockwall,
Hunt, Fannin, Grayson) and Kaufman and Van Zandt, so stops just outside the county can be routed. Any Debian or Ubuntu server with Docker works.

Outbound access is needed to: download.geofabrik.de (weekly map data),
services1/services2.arcgis.com (CAD and county data), geocoding.geo.census.gov
(fallback geocoding for addresses with no CAD parcel), build.protomaps.com (weekly map
tiles) and github.com (the pmtiles tool, once). Visitors' browsers only ever talk to
this server.

## First deploy

```sh
# On the server, as a user in the docker group
sudo apt install docker.io docker-compose osmium-tool git
git clone <repo> /opt/county-router && cd /opt/county-router

cp .env.production.example .env
nano .env      # DOMAIN

data/build.sh                                   # roads, geocoding data, map tiles: 5–10 min
docker compose build
docker compose run --rm --user "$(id -u):$(id -g)" cad-refresh   # CAD data, about 25 min
docker compose up -d
ops/install-automation.sh                       # cron, log rotation, security updates
```

Point DNS for both the domain and `www` at the server before starting; Caddy gets
certificates on first start. Plain `http://` redirects to HTTPS (308) and `www` to
`https://<domain>` (301). With Cloudflare, use "DNS only" until the first certificate is
issued (see [Behind Cloudflare](#behind-cloudflare)). Nominatim imports for up to 35
minutes after the first start; CAD lookups, Census lookups and routing work before it
finishes.

Instead of building data on the server, you can copy `data/out/` from a machine that
already has it (`rsync -a data/out/ server:/opt/county-router/data/out/`).

## Updating the app

```sh
git pull
docker compose up -d --build
```

## Running unattended

`ops/install-automation.sh` (run once, needs sudo) sets up everything below. After that
the server needs no attention: the VM, Docker and every service restart on their own.

| What | When | Script |
| --- | --- | --- |
| CAD parcels and county address points (one request every 5 s, about 25 min) | nightly 02:30 | `ops/refresh-cad.sh` |
| Roads, street geocoding and map tiles, built into a new folder and swapped in only when complete; last week's kept in `data/out.prev` | Sundays 03:00 | `ops/refresh-osm.sh` |
| Start any stopped service, restart any unhealthy one | every 5 min | `ops/watchdog.sh` |
| Rotate `logs/*.log` (8 weeks kept); container logs are capped at 30 MB each | weekly | logrotate, `compose.yaml` |
| Debian security updates, rebooting at 04:30 when an update needs it | daily | unattended-upgrades |

The two refresh jobs share a lock, so they never run at once, and the watchdog stays
out of their way. A failed CAD download keeps the old file; a failed map rebuild keeps
the old data. The weekly re-import leaves street-only lookups unavailable for about
35 minutes; CAD, Census and routing keep working.

**Alerts (optional).** Set `CAD_PING_URL`, `OSM_PING_URL` and `WATCHDOG_PING_URL` in `.env`
to check URLs from a monitoring service such as healthchecks.io. Each job pings the URL
when it succeeds and `<URL>/fail` when it fails, so a missed or failed run raises an
alert. An external uptime monitor on `https://<domain>/api/health` covers the rest.

Logs are in `logs/`, including `requests.log`: one JSON line per lookup or route with
the time, a request ID, the visitor's address, what was asked and what was answered.
The route page and print sheet show the request ID ("Route ref"), so a problem report
can name the exact request: `grep <id> logs/requests.log | jq`. The app runs as the
server user (`APP_UID`/`APP_GID` in `.env`, default 1000) so it can write there.

Nothing needs backing up: all data is rebuilt from public sources,
and `.env` holds only the domain.

## Behind Cloudflare

The app works with Cloudflare's proxy (orange cloud) in front:

- **SSL/TLS mode "Full (strict)".** Caddy keeps its own Let's Encrypt certificate.
- **Leave "Always Use HTTPS" off.** Caddy already redirects to HTTPS, and certificate
  renewals come in over plain HTTP.
- **Cache rule** for `/tiles/*`: "Eligible for cache", respecting origin cache headers.
  Cloudflare does not cache `.pmtiles` files by default; everything else follows the
  `Cache-Control` headers set in the `Caddyfile`.
- **Rate limiting still works per visitor.** Caddy trusts forwarded addresses only
  from Cloudflare's published ranges (listed in the `Caddyfile`; refresh the list if
  Cloudflare adds ranges).

## Checks

```sh
docker compose ps                       # all services up; api and vroom healthy
curl -u user:pass https://<host>/api/health   # ok: true, with the data build date
```

## Limits and privacy

- **Stop order.** Routes of up to 20 stops get the fastest possible order (exact search,
  under a second). Larger routes start from VROOM's order and keep improving it for
  `ROUTE_SEARCH_MS` (default 1500); at most `ROUTE_PARALLEL` (default 2) orderings run
  at once.
- **Rate limit.** `/api/geocode` and `/api/solve` allow a burst of 20 requests per visitor,
  refilled at one every 2 s (`RATE_LIMIT_BURST`, `RATE_LIMIT_REFILL_MS`), and at most 250
  stops per request.
- **No login.** Anyone with the link can use the site and its API. To restrict it again, add a `basic_auth` block to the Caddyfile.
- **Privacy.** Addresses with no CAD parcel are sent to the Census Geocoder. Owner names
  are never fetched or stored.
