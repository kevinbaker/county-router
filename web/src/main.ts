import * as maplibregl from "maplibre-gl";
import type { GeoJSONSource, LngLatBoundsLike } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import "./style.css";

// Vite moves MapLibre's worker when bundling; point MapLibre at the bundled copy.
maplibregl.setWorkerUrl(workerUrl);

type LatLon = { lat: number; lon: number };
type Confidence = "high" | "low" | "none";

type GeocodeResult = {
  input: string;
  confidence: Confidence;
  location: LatLon | null;
  matched: string | null;
  source: string | null;
  in_county: boolean;
};

type Stop = GeocodeResult & { moved: boolean; marker?: maplibregl.Marker };

type SolveResponse = {
  order: number[];
  legs: { distance_m: number; duration_s: number }[];
  total_distance_m: number;
  total_drive_s: number;
  total_s: number;
  unassigned: number[];
  geometry: GeoJSON.LineString;
};

type Plan = { start: Stop; ordered: Stop[]; roundTrip: boolean; dwellMin: number; result: SolveResponse };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let stops: Stop[] = [];
let placing: number | null = null;
let plan: Plan | null = null;
let county: GeoJSON.Polygon[] = [];

// ---------------------------------------------------------------- map

const map = new maplibregl.Map({
  container: "map",
  // Development basemap. Replace with the self-hosted PMTiles build before real use.
  style: {
    version: 8,
    sources: {
      osm: {
        type: "raster",
        tiles: ["https://tile.openstreetmap.org/{z}/{x}/{y}.png"],
        tileSize: 256,
        maxzoom: 19,
        attribution: "© OpenStreetMap contributors",
      },
    },
    layers: [{ id: "osm", type: "raster", source: "osm" }],
  },
  center: [-96.58, 33.19],
  zoom: 9.3,
});
map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");

map.on("load", async () => {
  const gj = await fetch("/api/boundary").then((r) => r.json());
  const geom = gj.geometry ?? gj;
  county =
    geom.type === "MultiPolygon"
      ? geom.coordinates.map((c: GeoJSON.Position[][]) => ({ type: "Polygon", coordinates: c }))
      : [geom];
  map.addSource("county", { type: "geojson", data: gj });
  map.addLayer({ id: "county-fill", type: "fill", source: "county", paint: { "fill-color": "#2563eb", "fill-opacity": 0.04 } });
  map.addLayer({ id: "county-line", type: "line", source: "county", paint: { "line-color": "#2563eb", "line-width": 1.5, "line-dasharray": [3, 2] } });

  map.addSource("route", { type: "geojson", data: emptyLine() });
  map.addLayer({ id: "route-casing", type: "line", source: "route", layout: { "line-join": "round", "line-cap": "round" }, paint: { "line-color": "#ffffff", "line-width": 8 } });
  map.addLayer({ id: "route-line", type: "line", source: "route", layout: { "line-join": "round", "line-cap": "round" }, paint: { "line-color": "#1d4ed8", "line-width": 4.5 } });
});

map.on("click", (e) => {
  if (placing === null) return;
  const s = stops[placing];
  s.location = { lat: e.lngLat.lat, lon: e.lngLat.lng };
  s.moved = true;
  s.in_county = inCounty(s.location);
  placing = null;
  map.getCanvas().style.cursor = "";
  renderReview();
});

function emptyLine(): GeoJSON.Feature<GeoJSON.LineString> {
  return { type: "Feature", properties: {}, geometry: { type: "LineString", coordinates: [] } };
}

// ---------------------------------------------------------------- step 1: input

/** One address per line; a single line holding several addresses is split after each ZIP code. */
export function parseAddresses(text: string): string[] {
  return text
    .split(/\r?\n/)
    .flatMap((line) => line.split(/(?<=\b\d{5}(?:-\d{4})?)\s*[,;]\s*/))
    .map((s) => s.trim().replace(/^["']+|["']+$/g, "").trim())
    .filter((s) => s.length > 0);
}

$("find").addEventListener("click", async () => {
  const addresses = parseAddresses($<HTMLTextAreaElement>("addresses").value);
  const err = $("input-error");
  err.hidden = true;
  if (addresses.length === 0) return showError(err, "Paste at least one address.");
  if (addresses.length > 250) return showError(err, `${addresses.length} stops; the limit is 250.`);

  const btn = $<HTMLButtonElement>("find");
  btn.disabled = true;
  btn.textContent = `Finding ${addresses.length} addresses…`;
  try {
    const results: GeocodeResult[] = await postJson("/api/geocode", { addresses });
    clearMarkers();
    stops = results.map((r) => ({ ...r, moved: false }));
    clearRoute();
    show("review");
    renderReview();
    fitToStops(stops);
  } catch (e) {
    showError(err, `Could not look up addresses: ${(e as Error).message}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "Find addresses";
  }
});

// ---------------------------------------------------------------- step 2: review

function usable(s: Stop): boolean {
  return s.location !== null && s.in_county;
}

function status(s: Stop): { cls: string; text: string } {
  if (!s.location) return { cls: "bad", text: "Not found" };
  if (!s.in_county) return { cls: "bad", text: "Outside county" };
  if (s.moved) return { cls: "moved", text: "Pin set by you" };
  if (s.confidence === "low") return { cls: "warn", text: "Street only, check pin" };
  return { cls: "ok", text: "Exact" };
}

function renderReview() {
  const list = $("stop-list");
  list.innerHTML = "";
  stops.forEach((s, i) => {
    const st = status(s);
    const li = document.createElement("li");
    li.className = `stop ${st.cls}${placing === i ? " placing" : ""}`;
    li.innerHTML = `
      <span class="num">${i + 1}</span>
      <div class="body">
        <div class="addr">${esc(s.input)}</div>
        ${s.matched && !s.moved ? `<div class="matched">${esc(s.matched)}</div>` : ""}
        <span class="chip ${st.cls}">${st.text}</span>
        ${!usable(s) ? `<button class="link place" data-i="${i}">${placing === i ? "Click the map…" : "Place on map"}</button>` : ""}
      </div>`;
    li.addEventListener("click", (e) => {
      const t = e.target as HTMLElement;
      if (t.classList.contains("place")) {
        placing = placing === i ? null : i;
        map.getCanvas().style.cursor = placing === null ? "" : "crosshair";
        renderReview();
        return;
      }
      if (s.location) map.flyTo({ center: [s.location.lon, s.location.lat], zoom: Math.max(map.getZoom(), 14) });
    });
    list.appendChild(li);
    placeMarker(s, String(i + 1), st.cls);
  });

  const ok = stops.filter(usable).length;
  const check = stops.filter((s) => usable(s) && s.confidence === "low" && !s.moved).length;
  $("review-summary").textContent =
    `${ok} of ${stops.length} ready` + (check ? ` · ${check} to check` : "");

  // Start options: every usable stop, keeping the current choice when possible.
  const sel = $<HTMLSelectElement>("start");
  const prev = sel.value;
  sel.innerHTML = stops
    .map((s, i) => (usable(s) ? `<option value="${i}">${i + 1}. ${esc(s.input)}</option>` : ""))
    .join("");
  if ([...sel.options].some((o) => o.value === prev)) sel.value = prev;
  $<HTMLButtonElement>("solve").disabled = ok < 2;
}

function placeMarker(s: Stop, label: string, cls: string) {
  if (!s.location) {
    s.marker?.remove();
    s.marker = undefined;
    return;
  }
  if (!s.marker) {
    const el = document.createElement("div");
    s.marker = new maplibregl.Marker({ element: el, draggable: true })
      .setLngLat([s.location.lon, s.location.lat])
      .addTo(map);
    s.marker.on("dragend", () => {
      const p = s.marker!.getLngLat();
      s.location = { lat: p.lat, lon: p.lng };
      s.moved = true;
      s.in_county = inCounty(s.location);
      if (plan) {
        clearRoute();
        show("review");
      }
      renderReview();
    });
  }
  s.marker.setLngLat([s.location.lon, s.location.lat]);
  // Toggle our classes only: MapLibre positions the pin through its own marker classes.
  const el = s.marker.getElement();
  el.classList.remove("ok", "warn", "bad", "moved", "start", "dim");
  el.classList.add("pin", cls);
  el.textContent = label;
}

// ---------------------------------------------------------------- step 3: solve

$("solve").addEventListener("click", async () => {
  const err = $("solve-error");
  err.hidden = true;
  const startIdx = Number($<HTMLSelectElement>("start").value);
  const start = stops[startIdx];
  const others = stops.filter((s, i) => usable(s) && i !== startIdx);
  const roundTrip = $<HTMLInputElement>("round-trip").checked;
  const dwellMin = Math.max(0, Number($<HTMLInputElement>("dwell").value) || 0);

  const btn = $<HTMLButtonElement>("solve");
  btn.disabled = true;
  btn.textContent = "Planning…";
  try {
    const result: SolveResponse = await postJson("/api/solve", {
      start: start.location,
      stops: others.map((s) => s.location),
      round_trip: roundTrip,
      dwell_minutes: dwellMin,
    });
    plan = { start, ordered: result.order.map((i) => others[i]), roundTrip, dwellMin, result };
    renderPlan();
  } catch (e) {
    showError(err, `Could not plan the route: ${(e as Error).message}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "Plan route";
  }
});

function renderPlan() {
  if (!plan) return;
  const { start, ordered, roundTrip, dwellMin, result } = plan;
  show("result");

  $("totals").innerHTML = `
    <div><strong>${miles(result.total_distance_m)}</strong><span>miles</span></div>
    <div><strong>${hm(result.total_drive_s)}</strong><span>driving</span></div>
    <div><strong>${hm(result.total_s)}</strong><span>with ${dwellMin} min stops</span></div>`;

  const rows: string[] = [`<li class="leg-start"><span class="num s">S</span><div class="body"><div class="addr">${esc(start.input)}</div><span class="muted">Start</span></div></li>`];
  let elapsed = 0;
  ordered.forEach((s, k) => {
    const leg = result.legs[k];
    elapsed += leg.duration_s;
    rows.push(`<li><span class="num">${k + 1}</span><div class="body">
      <div class="addr">${esc(s.input)}</div>
      <span class="muted">${miles(leg.distance_m)} mi · ${mins(leg.duration_s)} · arrive +${hm(elapsed)}</span></div></li>`);
    elapsed += dwellMin * 60;
  });
  if (roundTrip) {
    const leg = result.legs[ordered.length];
    elapsed += leg.duration_s;
    rows.push(`<li class="leg-start"><span class="num s">S</span><div class="body"><div class="addr">Back to start</div>
      <span class="muted">${miles(leg.distance_m)} mi · ${mins(leg.duration_s)} · arrive +${hm(elapsed)}</span></div></li>`);
  }
  if (result.unassigned.length) {
    rows.push(`<li class="bad"><div class="body">${result.unassigned.length} stop(s) could not be routed.</div></li>`);
  }
  $("route-list").innerHTML = rows.join("");

  // Renumber pins by visit order; hide stops that aren't in the route.
  const visit = new Map<Stop, string>([[start, "S"], ...ordered.map((s, k) => [s, String(k + 1)] as [Stop, string])]);
  for (const s of stops) {
    const label = visit.get(s);
    if (label) placeMarker(s, label, label === "S" ? "start" : "ok");
    else s.marker?.getElement().classList.add("dim");
  }

  (map.getSource("route") as GeoJSONSource).setData({ type: "Feature", properties: {}, geometry: result.geometry });
  fitToCoords(result.geometry.coordinates);
  renderExports();
}

function clearRoute() {
  plan = null;
  (map.getSource("route") as GeoJSONSource | undefined)?.setData(emptyLine());
  $("step-result").hidden = true;
}

$("edit").addEventListener("click", () => {
  clearRoute();
  show("review");
  renderReview();
});

// ---------------------------------------------------------------- exports

function routeStops(p: Plan): Stop[] {
  const list = [p.start, ...p.ordered];
  if (p.roundTrip) list.push(p.start);
  return list;
}

/**
 * What to send Google for a stop: the address text, so Google places it at the building
 * with its own data, unless the user set the pin by hand; then the pin is the truth.
 */
function googlePlace(s: Stop): string {
  if (s.moved || !s.location) return `${s.location!.lat.toFixed(6)},${s.location!.lon.toFixed(6)}`;
  // Census returns a cleaned-up form ("100 N 4TH ST, PRINCETON, TX, 75407").
  return s.source === "census" && s.matched ? s.matched : s.input;
}

/** Google Maps directions take an origin, a destination and at most 9 waypoints per link. */
function renderExports() {
  if (!plan) return;
  const pts = routeStops(plan).map(googlePlace);
  const links: string[] = [];
  for (let i = 0; i < pts.length - 1; i += 10) {
    const chunk = pts.slice(i, i + 11);
    const q = new URLSearchParams({
      api: "1",
      travelmode: "driving",
      origin: chunk[0],
      destination: chunk[chunk.length - 1],
    });
    const mid = chunk.slice(1, -1);
    if (mid.length) q.set("waypoints", mid.join("|"));
    links.push(`https://www.google.com/maps/dir/?${q}`);
  }
  $("gmaps-links").innerHTML = links
    .map((u, k) => `<a class="button" href="${u}" target="_blank" rel="noopener">Google Maps${links.length > 1 ? ` (part ${k + 1} of ${links.length})` : ""}</a>`)
    .join("");
}

$("gpx").addEventListener("click", () => {
  if (!plan) return;
  const wpts = [plan.start, ...plan.ordered]
    .map((s, k) => `  <wpt lat="${s.location!.lat}" lon="${s.location!.lon}"><name>${k === 0 ? "Start" : k}. ${xml(s.input)}</name></wpt>`)
    .join("\n");
  const trk = plan.result.geometry.coordinates.map(([lon, lat]) => `<trkpt lat="${lat}" lon="${lon}"/>`).join("");
  const gpx = `<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="county-router" xmlns="http://www.topografix.com/GPX/1/1">
${wpts}
  <trk><name>Route</name><trkseg>${trk}</trkseg></trk>
</gpx>
`;
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([gpx], { type: "application/gpx+xml" }));
  a.download = "route.gpx";
  a.click();
  URL.revokeObjectURL(a.href);
});

// ---------------------------------------------------------------- helpers

function show(step: "review" | "result") {
  $("step-review").hidden = step !== "review";
  $("step-result").hidden = step !== "result";
  $("step-input").hidden = step === "result";
}

function clearMarkers() {
  for (const s of stops) s.marker?.remove();
}

function fitToStops(list: Stop[]) {
  fitToCoords(list.filter((s) => s.location).map((s) => [s.location!.lon, s.location!.lat]));
}

function fitToCoords(coords: GeoJSON.Position[]) {
  if (coords.length === 0) return;
  const b = new maplibregl.LngLatBounds();
  for (const c of coords) b.extend(c as [number, number]);
  map.fitBounds(b as LngLatBoundsLike, { padding: 60, maxZoom: 15, duration: 600 });
}

/** Ray casting against the outer rings of the county outline. */
function inCounty(p: LatLon): boolean {
  return county.some((poly) => {
    const ring = poly.coordinates[0];
    let inside = false;
    for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
      const [xi, yi] = ring[i];
      const [xj, yj] = ring[j];
      if (yi > p.lat !== yj > p.lat && p.lon < ((xj - xi) * (p.lat - yi)) / (yj - yi) + xi) inside = !inside;
    }
    return inside;
  });
}

async function postJson<T>(url: string, body: unknown): Promise<T> {
  const r = await fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.error ?? `${r.status} ${r.statusText}`);
  return data as T;
}

function showError(el: HTMLElement, msg: string) {
  el.textContent = msg;
  el.hidden = false;
}

const miles = (m: number) => (m / 1609.344).toFixed(1);
const mins = (s: number) => `${Math.max(1, Math.round(s / 60))} min`;
function hm(s: number): string {
  const m = Math.round(s / 60);
  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}
function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}
const xml = esc;
