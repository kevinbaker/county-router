import * as maplibregl from "maplibre-gl";
import type { GeoJSONSource, LngLatBoundsLike } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import { layers, namedFlavor } from "@protomaps/basemaps";
import { Protocol } from "pmtiles";
import "./style.css";

// Vite moves MapLibre's worker when bundling; point MapLibre at the bundled copy.
maplibregl.setWorkerUrl(workerUrl);
// Map tiles come from one PMTiles file on our own server, read with range requests.
maplibregl.addProtocol("pmtiles", new Protocol().tile);

type LatLon = { lat: number; lon: number };
type Confidence = "high" | "low" | "none";

type GeocodeResult = {
  input: string;
  confidence: Confidence;
  /** Where to show the stop. */
  location: LatLon | null;
  /** Point on the addressed street to route to, when known. */
  route_location: LatLon | null;
  matched: string | null;
  source: "cad" | "census" | "nominatim" | null;
  /** Inside Collin County. */
  in_county: boolean;
  /** Inside the routable region: Collin and its neighbouring counties. */
  in_area: boolean;
  prop_id: number | null;
  method: "building" | "frontage" | "lot_centre" | null;
  parcel: GeoJSON.Geometry | null;
  note: string | null;
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

/** One row of the route: a stop in visit order, with the drive that reaches it. */
type Visit = {
  stop: Stop;
  label: string;
  name: string;
  leg: { distance_m: number; duration_s: number } | null;
  /** Seconds from departure to arrival. */
  arrive_s: number | null;
  /** The drive back to the start at the end of a round trip. */
  return: boolean;
};

/** How the route finishes: back at the start, at a chosen stop, or wherever is shortest. */
type Finish = "start" | "end" | "open";

type Plan = {
  start: Stop;
  ordered: Stop[];
  finish: Finish;
  /** The chosen last stop when `finish` is "end". */
  end: Stop | null;
  dwellMin: number;
  result: SolveResponse;
};

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let stops: Stop[] = [];
let placing: number | null = null;
let plan: Plan | null = null;
/** Route-page rows in visit order; list items point into this by index. */
let visits: Visit[] = [];
/** The stop highlighted in the list and on the map. */
let current: Stop | null = null;
let county: GeoJSON.Polygon[] = [];
let region: GeoJSON.Polygon[] = [];

// ---------------------------------------------------------------- map

const map = new maplibregl.Map({
  container: "map",
  // Self-hosted basemap: region tiles, fonts and icons all come from this site.
  style: {
    version: 8,
    glyphs: `${location.origin}/basemap/fonts/{fontstack}/{range}.pbf`,
    sprite: `${location.origin}/basemap/sprites/v4/light`,
    sources: {
      protomaps: {
        type: "vector",
        url: `pmtiles://${location.origin}/tiles/region.pmtiles`,
        attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> · <a href="https://protomaps.com">Protomaps</a>',
      },
    },
    layers: layers("protomaps", namedFlavor("light"), { lang: "en" }),
  },
  center: [-96.58, 33.19],
  zoom: 9.3,
});
map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");

map.on("load", async () => {
  const [gj, regionGj] = await Promise.all([
    fetch("/api/boundary").then((r) => r.json()),
    fetch("/api/region").then((r) => r.json()),
  ]);
  county = polygons(gj);
  region = polygons(regionGj);
  // The area stops can be in, faintly; Collin itself on top.
  map.addSource("region", { type: "geojson", data: regionGj });
  map.addLayer({ id: "region-line", type: "line", source: "region", paint: { "line-color": "#64748b", "line-width": 1, "line-opacity": 0.5, "line-dasharray": [2, 3] } });
  map.addSource("county", { type: "geojson", data: gj });
  map.addLayer({ id: "county-fill", type: "fill", source: "county", paint: { "fill-color": "#2563eb", "fill-opacity": 0.04 } });
  map.addLayer({ id: "county-line", type: "line", source: "county", paint: { "line-color": "#2563eb", "line-width": 1.5, "line-dasharray": [3, 2] } });

  map.addSource("parcels", { type: "geojson", data: { type: "FeatureCollection", features: [] } });
  map.addLayer({ id: "parcel-fill", type: "fill", source: "parcels", paint: { "fill-color": "#15803d", "fill-opacity": 0.1 } });
  map.addLayer({ id: "parcel-line", type: "line", source: "parcels", paint: { "line-color": "#15803d", "line-width": 1.5 } });

  map.addSource("route", { type: "geojson", data: emptyLine() });
  map.addLayer({ id: "route-casing", type: "line", source: "route", layout: { "line-join": "round", "line-cap": "round" }, paint: { "line-color": "#ffffff", "line-width": 8 } });
  map.addLayer({ id: "route-line", type: "line", source: "route", layout: { "line-join": "round", "line-cap": "round" }, paint: { "line-color": "#1d4ed8", "line-width": 4.5 } });
});

map.on("click", (e) => {
  if (placing === null) return;
  const s = stops[placing];
  s.location = { lat: e.lngLat.lat, lon: e.lngLat.lng };
  s.moved = true;
  s.route_location = null;
  s.in_county = inCounty(s.location);
  s.in_area = inRegion(s.location);
  placing = null;
  map.getCanvas().style.cursor = "";
  renderReview();
});

function emptyLine(): GeoJSON.Feature<GeoJSON.LineString> {
  return { type: "Feature", properties: {}, geometry: { type: "LineString", coordinates: [] } };
}

// ---------------------------------------------------------------- step 1: input

/**
 * One stop per line. A line of only numbers is a list of CAD property IDs; a line holding
 * several addresses is split after each ZIP code.
 */
export function parseStops(text: string): string[] {
  return text
    .split(/\r?\n/)
    .flatMap((line) =>
      /^[\s\d#,;]+$/.test(line) ? line.split(/[\s,;]+/) : line.split(/(?<=\b\d{5}(?:-\d{4})?)\s*[,;]\s*/),
    )
    .map((s) => s.trim().replace(/^["']+|["']+$/g, "").trim())
    .filter((s) => s.length > 0);
}

/** Collin CAD office (by property ID), McKinney City Hall and the county courthouse. */
const EXAMPLE_STOPS = [
  "2625294",
  "401 E Virginia St, McKinney, TX 75069",
  "2100 Bloomdale Rd, McKinney, TX 75071",
];

$("example").addEventListener("click", () => {
  const box = $<HTMLTextAreaElement>("addresses");
  box.value = EXAMPLE_STOPS.join("\n");
  box.focus();
});

$("find").addEventListener("click", async () => {
  const addresses = parseStops($<HTMLTextAreaElement>("addresses").value);
  const err = $("input-error");
  err.hidden = true;
  if (addresses.length === 0) return showError(err, "Enter at least one property ID or address.");
  if (addresses.length > 250) return showError(err, `${addresses.length} stops; the limit is 250.`);

  const btn = $<HTMLButtonElement>("find");
  btn.disabled = true;
  btn.textContent = `Finding ${addresses.length} stops…`;
  try {
    const results: GeocodeResult[] = await postJson("/api/geocode", { addresses });
    clearMarkers();
    stops = results.map((r) => ({ ...r, moved: false }));
    clearRoute();
    show("review");
    renderReview();
    fitToStops(stops);
  } catch (e) {
    showError(err, `Could not look up stops: ${(e as Error).message}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "Find stops";
  }
});

// ---------------------------------------------------------------- step 2: review

function usable(s: Stop): boolean {
  return s.location !== null && s.in_area;
}

function status(s: Stop): { cls: string; text: string } {
  if (!s.location) return { cls: "bad", text: "Not found" };
  if (!s.in_area) return { cls: "bad", text: "Outside service area" };
  if (s.moved) return { cls: "moved", text: "Pin set by you" };
  if (s.method === "building") return { cls: "ok", text: "Building" };
  if (s.method === "frontage") return { cls: "ok", text: "Lot frontage" };
  if (s.method === "lot_centre") return { cls: "warn", text: "Lot centre, check pin" };
  if (s.confidence === "low") return { cls: "warn", text: "Street only, check pin" };
  return { cls: "ok", text: "Address" };
}

/** The stop's name: the CAD site address when there is a parcel, else what was typed. */
/** The property ID, when the user typed it; stops entered as addresses don't show one. */
function enteredId(s: Stop): string {
  if (s.prop_id === null || !new RegExp(`(^|\\D)${s.prop_id}($|\\D)`).test(s.input)) return "";
  return `<span class="prop-id">ID ${s.prop_id}</span>`;
}

function title(s: Stop): string {
  return s.source === "cad" && s.matched ? s.matched : s.input;
}

/** Second line: why a stop wasn't found, or the matched address for typed addresses. */
function detail(s: Stop): string {
  if (s.source === "cad") return "";
  if (s.note) return s.note;
  return s.matched && !s.moved ? s.matched : "";
}

function renderReview() {
  const list = $("stop-list");
  list.innerHTML = "";
  stops.forEach((s, i) => {
    const st = status(s);
    const li = document.createElement("li");
    li.className = `stop ${st.cls}${placing === i ? " placing" : ""}${s === current ? " selected" : ""}`;
    li.dataset.i = String(i);
    li.innerHTML = `
      <span class="num">${i + 1}</span>
      <div class="body">
        <div class="addr">${esc(title(s))}${enteredId(s)}</div>
        ${detail(s) ? `<div class="matched">${esc(detail(s))}</div>` : ""}
        <span class="chips"><span class="chip ${st.cls}">${st.text}</span>${usable(s) && !s.in_county ? `<span class="chip note">Outside Collin County</span>` : ""}</span>
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
      select(s, "list");
    });
    list.appendChild(li);
    placeMarker(s, String(i + 1), st.cls);
  });

  (map.getSource("parcels") as GeoJSONSource | undefined)?.setData({
    type: "FeatureCollection",
    features: stops
      .filter((s) => s.parcel)
      .map((s) => ({ type: "Feature", properties: {}, geometry: s.parcel! })),
  });

  const ok = stops.filter(usable).length;
  const check = stops.filter((s) => usable(s) && s.confidence === "low" && !s.moved).length;
  $("review-summary").textContent =
    `${ok} of ${stops.length} ready` + (check ? ` · ${check} to check` : "");

  // Start options: every usable stop, keeping the current choice when possible.
  const sel = $<HTMLSelectElement>("start");
  const prev = sel.value;
  sel.innerHTML = stops
    .map((s, i) => (usable(s) ? `<option value="${i}">${i + 1}. ${esc(title(s))}</option>` : ""))
    .join("");
  if ([...sel.options].some((o) => o.value === prev)) sel.value = prev;
  renderEndOptions();
  $<HTMLButtonElement>("solve").disabled = ok < 2;
}

/** "End at" choices: every usable stop except the start; defaults to the last one. */
function renderEndOptions() {
  const sel = $<HTMLSelectElement>("end");
  const prev = sel.value;
  const startIdx = $<HTMLSelectElement>("start").value;
  sel.innerHTML = stops
    .map((s, i) => (usable(s) && String(i) !== startIdx ? `<option value="${i}">${i + 1}. ${esc(title(s))}</option>` : ""))
    .join("");
  const options = [...sel.options];
  sel.value = options.some((o) => o.value === prev) ? prev : (options.at(-1)?.value ?? "");
}

$("start").addEventListener("change", renderEndOptions);
// Picking an end stop means ending there.
$("end").addEventListener("change", () => {
  document.querySelector<HTMLInputElement>('input[name="finish"][value="end"]')!.checked = true;
});

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
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      select(s, "map");
    });
    s.marker.on("dragend", () => {
      const p = s.marker!.getLngLat();
      s.location = { lat: p.lat, lon: p.lng };
      s.moved = true;
      s.route_location = null;
      s.in_county = inCounty(s.location);
      s.in_area = inRegion(s.location);
  s.in_area = inRegion(s.location);
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
  const finish = (document.querySelector<HTMLInputElement>('input[name="finish"]:checked')?.value ?? "start") as Finish;
  const endIdx = $<HTMLSelectElement>("end").value;
  const end = finish === "end" && endIdx !== "" ? stops[Number(endIdx)] : null;
  if (finish === "end" && !end) return showError(err, "Pick a stop to end at.");
  const others = stops.filter((s, i) => usable(s) && i !== startIdx && s !== end);
  const dwellMin = Math.max(0, Number($<HTMLInputElement>("dwell").value) || 0);

  const btn = $<HTMLButtonElement>("solve");
  btn.disabled = true;
  btn.textContent = "Planning…";
  try {
    const result: SolveResponse = await postJson("/api/solve", {
      start: routePoint(start),
      stops: others.map(routePoint),
      round_trip: finish === "start",
      end: end ? routePoint(end) : null,
      dwell_minutes: dwellMin,
    });
    plan = { start, ordered: result.order.map((i) => others[i]), finish, end, dwellMin, result };
    renderPlan();
  } catch (e) {
    showError(err, `Could not plan the route: ${(e as Error).message}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "Plan route";
  }
});

/** Route to the addressed street when known, so a long lot isn't reached from its back road. */
function routePoint(s: Stop): LatLon {
  return s.route_location ?? s.location!;
}

function renderPlan() {
  if (!plan) return;
  const { dwellMin, result } = plan;
  show("result");

  $("totals").innerHTML = `
    <div><strong>${miles(result.total_distance_m)}</strong><span>miles</span></div>
    <div><strong>${hm(result.total_drive_s)}</strong><span>driving</span></div>
    <div><strong>${hm(result.total_s)}</strong><span>with ${dwellMin} min stops</span></div>`;

  visits = planVisits(plan);
  const rows = visits.map((v, k) => {
    const cls = v.label === "S" ? "leg-start" : "";
    const sub = v.leg
      ? `${miles(v.leg.distance_m)} mi · ${mins(v.leg.duration_s)} · arrive +${hm(v.arrive_s!)}`
      : "Start";
    return `<li class="${cls}" data-k="${k}"><span class="num${v.label === "S" ? " s" : ""}">${v.label}</span><div class="body">
      <div class="addr">${esc(v.name)}${v.return ? "" : enteredId(v.stop)}</div><span class="muted">${sub}</span></div></li>`;
  });
  if (result.unassigned.length) {
    rows.push(`<li class="bad"><div class="body">${result.unassigned.length} stop(s) could not be routed.</div></li>`);
  }
  $("route-list").innerHTML = rows.join("");

  // Renumber pins by visit order; hide stops that aren't in the route.
  const visit = new Map<Stop, string>(visits.filter((v) => !v.return).map((v) => [v.stop, v.label]));
  for (const s of stops) {
    const label = visit.get(s);
    if (label) placeMarker(s, label, label === "S" ? "start" : "ok");
    else s.marker?.getElement().classList.add("dim");
  }

  (map.getSource("route") as GeoJSONSource).setData({ type: "Feature", properties: {}, geometry: result.geometry });
  fitToCoords(result.geometry.coordinates);
  renderExports();
}

/** Start, each stop in order, then the return or the chosen end, with leg times and
 *  arrival offsets. */
function planVisits(p: Plan): Visit[] {
  const out: Visit[] = [{ stop: p.start, label: "S", name: title(p.start), leg: null, arrive_s: null, return: false }];
  let elapsed = 0;
  p.ordered.forEach((s, k) => {
    const leg = p.result.legs[k];
    elapsed += leg.duration_s;
    out.push({ stop: s, label: String(k + 1), name: title(s), leg, arrive_s: elapsed, return: false });
    elapsed += p.dwellMin * 60;
  });
  const last = p.result.legs[p.ordered.length];
  if (p.finish === "start" && last) {
    elapsed += last.duration_s;
    out.push({ stop: p.start, label: "S", name: "Back to start", leg: last, arrive_s: elapsed, return: true });
  } else if (p.end && last) {
    elapsed += last.duration_s;
    out.push({ stop: p.end, label: String(p.ordered.length + 1), name: title(p.end), leg: last, arrive_s: elapsed, return: false });
  }
  return out;
}

/** Highlights a stop in the list and on the map. From the list, the map zooms to it;
 *  from the map, the list scrolls to it. */
function select(s: Stop | null, from: "list" | "map" | null = null) {
  current = s;
  // Whichever list is showing: the route (rows point into `visits`) or the stop check
  // (rows point into `stops`).
  const items = plan
    ? [...$("route-list").querySelectorAll<HTMLElement>("li[data-k]")]
    : [...$("stop-list").querySelectorAll<HTMLElement>("li[data-i]")];
  const stopOf = (li: HTMLElement) => (plan ? visits[Number(li.dataset.k)]?.stop : stops[Number(li.dataset.i)]);
  for (const li of items) li.classList.toggle("selected", s !== null && stopOf(li) === s);
  for (const st of stops) st.marker?.getElement().classList.toggle("selected", st === s);
  if (!s) return;
  if (from === "list" && s.location) {
    map.flyTo({ center: [s.location.lon, s.location.lat], zoom: Math.max(map.getZoom(), 15.5) });
  }
  if (from === "map") items.find((li) => li.classList.contains("selected"))?.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

$("route-list").addEventListener("click", (e) => {
  const li = (e.target as HTMLElement).closest<HTMLElement>("li[data-k]");
  if (li) select(visits[Number(li.dataset.k)].stop, "list");
});

$("fit").addEventListener("click", () => {
  if (plan) fitToCoords(plan.result.geometry.coordinates);
  else fitToStops(stops);
});

function clearRoute() {
  select(null);
  visits = [];
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
  if (p.finish === "start") list.push(p.start);
  if (p.end) list.push(p.end);
  return list;
}

/**
 * What to send a maps app for a stop: the address text, so Google or Apple places it at
 * the building with its own data, unless the user set the pin by hand; then the pin is
 * the truth.
 */
function navPlace(s: Stop): string {
  if (s.moved || !s.location) return `${s.location!.lat.toFixed(6)},${s.location!.lon.toFixed(6)}`;
  // CAD and Census return a cleaned-up form ("100 N FOURTH ST, PRINCETON, TX 75407").
  return (s.source === "cad" || s.source === "census") && s.matched ? s.matched : s.input;
}

/**
 * The route cut into legs of at most 11 places each (a start, 9 stops, an end), each
 * leg starting where the previous one ended. Google Maps links take at most 9
 * waypoints; Apple doesn't document a limit, so it gets the same parts.
 */
function navLegs(p: Plan): string[][] {
  const pts = routeStops(p).map(navPlace);
  const legs: string[][] = [];
  for (let i = 0; i < pts.length - 1; i += 10) legs.push(pts.slice(i, i + 11));
  return legs;
}

function googleUrl(leg: string[]): string {
  const q = new URLSearchParams({
    api: "1",
    travelmode: "driving",
    origin: leg[0],
    destination: leg[leg.length - 1],
  });
  const mid = leg.slice(1, -1);
  if (mid.length) q.set("waypoints", mid.join("|"));
  return `https://www.google.com/maps/dir/?${q}`;
}

/** Apple Maps unified URL: opens the Maps app on iPhone, iPad and Mac (iOS 18.4+). */
function appleUrl(leg: string[]): string {
  // Spaces as %20 rather than URLSearchParams' "+", which Apple doesn't document.
  const param = (k: string, v: string) => `${k}=${encodeURIComponent(v)}`;
  const parts = [param("source", leg[0]), param("destination", leg[leg.length - 1]), "mode=driving"];
  for (const w of leg.slice(1, -1)) parts.push(param("waypoint", w));
  return `https://maps.apple.com/directions?${parts.join("&")}`;
}

function renderExports() {
  if (!plan) return;
  const legs = navLegs(plan);
  const buttons = (name: string, url: (leg: string[]) => string) =>
    legs
      .map((leg, k) => `<a class="button" href="${url(leg)}" target="_blank" rel="noopener">${name}${legs.length > 1 ? ` (part ${k + 1} of ${legs.length})` : ""}</a>`)
      .join("");
  $("gmaps-links").innerHTML = buttons("Google Maps", googleUrl) + buttons("Apple Maps", appleUrl);
}

$("gpx").addEventListener("click", () => {
  if (!plan) return;
  const wpts = [plan.start, ...plan.ordered]
    .map((s, k) => `  <wpt lat="${s.location!.lat}" lon="${s.location!.lon}"><name>${k === 0 ? "Start" : k}. ${xml(title(s))}</name></wpt>`)
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

// ---------------------------------------------------------------- print

$("print").addEventListener("click", async () => {
  if (!plan) return;
  const btn = $<HTMLButtonElement>("print");
  btn.disabled = true;
  btn.textContent = "Preparing…";
  try {
    const image = await routeSnapshot(plan);
    renderPrintSheet(plan, image);
    window.print();
  } finally {
    btn.disabled = false;
    btn.textContent = "Print route";
  }
});

/**
 * A PNG of the whole route with numbered stops. The pins are page elements, not part of
 * the map canvas, so they are drawn onto the copy here.
 */
async function routeSnapshot(p: Plan): Promise<string> {
  const camera = { center: map.getCenter(), zoom: map.getZoom() };
  const b = new maplibregl.LngLatBounds();
  for (const c of p.result.geometry.coordinates) b.extend(c as [number, number]);
  map.fitBounds(b, { padding: 50, maxZoom: 15, animate: false });
  await new Promise((r) => map.once("idle", r));
  // The WebGL buffer is only readable during the frame that drew it.
  const frame = await new Promise<HTMLCanvasElement>((resolve) => {
    map.once("render", () => resolve(map.getCanvas()));
    map.triggerRepaint();
  });
  const out = document.createElement("canvas");
  out.width = frame.width;
  out.height = frame.height;
  const ctx = out.getContext("2d")!;
  ctx.drawImage(frame, 0, 0);

  const scale = frame.width / frame.clientWidth;
  // Larger than on screen: the image is shrunk to fit the page.
  const r = 17 * scale;
  ctx.font = `700 ${17 * scale}px system-ui, sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  for (const v of planVisits(p)) {
    if (v.return || !v.stop.location) continue;
    const pt = map.project([v.stop.location.lon, v.stop.location.lat]);
    const [x, y] = [pt.x * scale, pt.y * scale];
    ctx.beginPath();
    ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fillStyle = v.label === "S" ? "#1c1f23" : "#15803d";
    ctx.fill();
    ctx.lineWidth = 2 * scale;
    ctx.strokeStyle = "#fff";
    ctx.stroke();
    ctx.fillStyle = "#fff";
    ctx.fillText(v.label, x, y + 0.5 * scale);
  }
  map.jumpTo(camera);
  return out.toDataURL("image/png");
}

function renderPrintSheet(p: Plan, image: string) {
  const r = p.result;
  const date = new Date().toLocaleDateString(undefined, { weekday: "long", year: "numeric", month: "long", day: "numeric" });
  const rows = planVisits(p)
    .map((v) => {
      const id = v.return ? "" : enteredId(v.stop).replace(/<[^>]+>/g, "").replace(/^ID /, "");
      return `<tr>
        <td class="seq">${v.label}</td>
        <td>${esc(v.name)}</td>
        <td class="id">${id}</td>
        <td class="leg">${v.leg ? `${miles(v.leg.distance_m)} mi · ${mins(v.leg.duration_s)}` : "Start"}</td>
        <td class="arrive">${v.arrive_s !== null ? `+${hm(v.arrive_s)}` : ""}</td>
        <td class="done">${v.label !== "S" ? '<span class="box"></span>' : ""}</td>
      </tr>`;
    })
    .join("");
  $("print-sheet").innerHTML = `
    <h1>Route: ${p.ordered.length + 1 + (p.end ? 1 : 0)} stops</h1>
    <div class="meta">Collin County · printed ${esc(date)}</div>
    <div class="totals-line">${miles(r.total_distance_m)} miles · ${hm(r.total_drive_s)} driving · ${hm(r.total_s)} with ${p.dwellMin} min per stop${p.finish === "start" ? " · returns to start" : p.end ? " · ends at a chosen stop" : " · ends at the last stop"}</div>
    <img src="${image}" alt="Route map" />
    <table>
      <thead><tr><th>#</th><th>Address</th><th>Property ID</th><th>Drive</th><th>Arrive</th><th>Done</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>`;
}

// ---------------------------------------------------------------- start over

/** Clearing loses the stops and pins, so the first click asks and the second clears. */
let confirmTimer: number | undefined;
$("start-over").addEventListener("click", () => {
  const btn = $("start-over");
  if (!btn.classList.contains("confirm")) {
    btn.classList.add("confirm");
    btn.textContent = "Clear everything?";
    confirmTimer = window.setTimeout(resetConfirm, 4000);
    return;
  }
  resetConfirm();
  startOver();
});

function resetConfirm() {
  window.clearTimeout(confirmTimer);
  const btn = $("start-over");
  btn.classList.remove("confirm");
  btn.textContent = "Start over";
}

function startOver() {
  clearRoute();
  clearMarkers();
  stops = [];
  placing = null;
  map.getCanvas().style.cursor = "";
  (map.getSource("parcels") as GeoJSONSource | undefined)?.setData({ type: "FeatureCollection", features: [] });
  $("stop-list").innerHTML = "";
  $<HTMLTextAreaElement>("addresses").value = "";
  for (const id of ["input-error", "solve-error", "step-review", "step-result", "fit", "start-over"]) $(id).hidden = true;
  $("step-input").hidden = false;
  map.flyTo({ center: [-96.58, 33.19], zoom: 9.3 });
  $<HTMLTextAreaElement>("addresses").focus();
}

// ---------------------------------------------------------------- helpers

function show(step: "review" | "result") {
  $("step-review").hidden = step !== "review";
  $("step-result").hidden = step !== "result";
  $("step-input").hidden = step === "result";
  $("fit").hidden = false;
  $("start-over").hidden = false;
  $("fit").querySelector("span")!.textContent = step === "result" ? "Show full route" : "Show all stops";
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

function polygons(gj: GeoJSON.Feature | GeoJSON.Geometry): GeoJSON.Polygon[] {
  const geom = ("geometry" in gj ? gj.geometry : gj) as GeoJSON.Polygon | GeoJSON.MultiPolygon;
  return geom.type === "MultiPolygon" ? geom.coordinates.map((c) => ({ type: "Polygon", coordinates: c })) : [geom];
}

function inCounty(p: LatLon): boolean {
  return inside(county, p);
}

function inRegion(p: LatLon): boolean {
  return inside(region, p);
}

/** Ray casting against the outer rings. */
function inside(polys: GeoJSON.Polygon[], p: LatLon): boolean {
  return polys.some((poly) => {
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
  if (r.status === 429) {
    const wait = Number(r.headers.get("retry-after")) || 30;
    throw new Error(`Too many requests. Please wait ${wait} seconds and try again.`);
  }
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
