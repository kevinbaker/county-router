/**
 * Drive mode: the planned route one stop at a time. Each stop goes to Google or Apple
 * Maps as a single destination from wherever the driver is, so the maps app ends the
 * trip at that stop instead of carrying on to the next waypoint. The driver switches
 * back here, marks the stop done and sends the next one.
 *
 * The trip is kept in localStorage: iOS often discards a backgrounded tab or home-screen
 * app while Maps is navigating, and the driver must come back to the same place.
 */

export type DriveStop = {
  label: string;
  name: string;
  /** What to send the maps app: coordinates or address text. */
  place: string;
  lat: number;
  lon: number;
  /** Planned drive from the previous stop. */
  leg_m: number | null;
  leg_s: number | null;
  status: "todo" | "done" | "skipped";
};

type MapsApp = "google" | "apple";

type Trip = {
  stops: DriveStop[];
  /** The stop being driven to. */
  index: number;
  /** Set when the driver left for the maps app, so their return can be checked. */
  navigating: boolean;
};

const TRIP_KEY = "drive-trip";
const APP_KEY = "drive-app";
/** Closer than this on returning from the maps app counts as arrived. */
const ARRIVED_M = 150;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let trip: Trip | null = load();

export function hasTrip(): boolean {
  return trip !== null;
}

/** Starts a new trip, replacing any unfinished one. */
export function startDrive(stops: Omit<DriveStop, "status">[]) {
  trip = { stops: stops.map((s) => ({ ...s, status: "todo" })), index: 0, navigating: false };
  save();
  openDrive();
}

export function openDrive() {
  if (!trip) return;
  $("drive").hidden = false;
  $("resume-drive").hidden = true;
  document.body.classList.add("driving");
  render();
}

function closeDrive() {
  $("drive").hidden = true;
  $("resume-drive").hidden = trip === null;
  document.body.classList.remove("driving");
}

function render() {
  if (!trip) return;
  const { stops, index } = trip;
  const done = stops.filter((s) => s.status !== "todo").length;
  const s = stops[index];
  const finished = stops.every((x) => x.status !== "todo");

  $("drive-progress").textContent = `${done} of ${stops.length} done`;
  $("drive-current").hidden = finished;
  $("drive-finished").hidden = !finished;
  if (!finished) {
    $("drive-label").textContent = s.label === "S" ? "Back to start" : `Stop ${s.label}`;
    $("drive-name").textContent = s.name;
    $("drive-leg").textContent =
      s.leg_m !== null && s.leg_s !== null ? `Planned ${miles(s.leg_m)} mi · ${mins(s.leg_s)} from the previous stop` : "";
    const app = mapsApp();
    const a = $<HTMLAnchorElement>("drive-go");
    a.href = app === "google" ? googleNav(s.place) : appleNav(s.place);
    a.textContent = `Navigate in ${app === "google" ? "Google Maps" : "Apple Maps"}`;
    for (const b of document.querySelectorAll<HTMLButtonElement>("#drive-app button")) {
      b.classList.toggle("on", b.dataset.app === app);
    }
    $("drive-here").hidden = true;
  }

  $("drive-list").innerHTML = stops
    .map((x, k) => {
      const cls = [x.status, k === index && !finished ? "current" : ""].join(" ");
      const state = x.status === "done" ? "Done" : x.status === "skipped" ? "Skipped" : k === index ? "Next" : "";
      return `<li class="${cls}" data-k="${k}"><span class="num${x.label === "S" ? " s" : ""}">${x.label}</span>
        <div class="body"><div class="addr">${esc(x.name)}</div>${state ? `<span class="muted">${state}</span>` : ""}</div></li>`;
    })
    .join("");
}

/** The next stop still to visit after `from`, wrapping round to any left earlier. */
function nextTodo(from: number): number {
  const n = trip!.stops.length;
  for (let k = 1; k <= n; k++) {
    const i = (from + k) % n;
    if (trip!.stops[i].status === "todo") return i;
  }
  return from;
}

function finishCurrent(status: "done" | "skipped") {
  if (!trip) return;
  trip.stops[trip.index].status = status;
  trip.index = nextTodo(trip.index);
  trip.navigating = false;
  save();
  render();
  $("drive-current").scrollIntoView({ block: "nearest" });
}

$("drive-done").addEventListener("click", () => finishCurrent("done"));
$("drive-skip").addEventListener("click", () => finishCurrent("skipped"));

$("drive-go").addEventListener("click", () => {
  if (!trip) return;
  trip.navigating = true;
  save();
});

$("drive-app").addEventListener("click", (e) => {
  const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-app]");
  if (!b) return;
  try {
    localStorage.setItem(APP_KEY, b.dataset.app!);
  } catch {}
  render();
});

/** Tapping a stop in the list makes it the next one; tapping a finished stop reopens it. */
$("drive-list").addEventListener("click", (e) => {
  const li = (e.target as HTMLElement).closest<HTMLElement>("li[data-k]");
  if (!li || !trip) return;
  const k = Number(li.dataset.k);
  trip.stops[k].status = "todo";
  trip.index = k;
  trip.navigating = false;
  save();
  render();
  $("drive").scrollTo({ top: 0, behavior: "smooth" });
});

$("drive-close").addEventListener("click", closeDrive);
$("resume-drive").addEventListener("click", openDrive);

/** Ending loses the progress, so the first click asks and the second ends. */
let endTimer: number | undefined;
$("drive-end").addEventListener("click", () => {
  const btn = $("drive-end");
  if (!btn.classList.contains("confirm")) {
    btn.classList.add("confirm");
    btn.textContent = "End this trip?";
    endTimer = window.setTimeout(resetEnd, 4000);
    return;
  }
  resetEnd();
  trip = null;
  try {
    localStorage.removeItem(TRIP_KEY);
  } catch {}
  closeDrive();
});

function resetEnd() {
  window.clearTimeout(endTimer);
  const btn = $("drive-end");
  btn.classList.remove("confirm");
  btn.textContent = "End trip";
}

/** Back from the maps app: if the driver is at the stop, say so and offer to mark it done. */
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible" || !trip?.navigating || $("drive").hidden) return;
  const target = trip.stops[trip.index];
  navigator.geolocation?.getCurrentPosition(
    (pos) => {
      if (!trip || trip.stops[trip.index] !== target) return;
      const d = metres(pos.coords.latitude, pos.coords.longitude, target.lat, target.lon);
      const here = $("drive-here");
      here.hidden = false;
      here.classList.toggle("arrived", d <= ARRIVED_M);
      here.textContent = d <= ARRIVED_M ? "You're at this stop." : `You're ${miles(d)} mi from this stop.`;
    },
    () => {},
    { enableHighAccuracy: true, timeout: 8000, maximumAge: 30000 },
  );
});

/**
 * Google Maps with no origin starts from the driver's location; dir_action=navigate
 * starts turn-by-turn straight away. Opens the Google Maps app when installed.
 */
function googleNav(place: string): string {
  const q = new URLSearchParams({ api: "1", destination: place, travelmode: "driving", dir_action: "navigate" });
  return `https://www.google.com/maps/dir/?${q}`;
}

/** Apple Maps with no source starts from the driver's location. */
function appleNav(place: string): string {
  return `https://maps.apple.com/directions?destination=${encodeURIComponent(place)}&mode=driving`;
}

/** The driver's choice, else Apple Maps on Apple devices and Google Maps elsewhere. */
function mapsApp(): MapsApp {
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(APP_KEY);
  } catch {}
  if (saved === "google" || saved === "apple") return saved;
  return /iPhone|iPad|Macintosh/.test(navigator.userAgent) ? "apple" : "google";
}

function load(): Trip | null {
  try {
    const t = JSON.parse(localStorage.getItem(TRIP_KEY) ?? "null") as Trip | null;
    return t && Array.isArray(t.stops) && t.stops.length ? t : null;
  } catch {
    return null;
  }
}

function save() {
  try {
    localStorage.setItem(TRIP_KEY, JSON.stringify(trip));
  } catch {}
}

function metres(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const r = Math.PI / 180;
  const a =
    Math.sin(((lat2 - lat1) * r) / 2) ** 2 +
    Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(((lon2 - lon1) * r) / 2) ** 2;
  return 2 * 6371000 * Math.asin(Math.sqrt(a));
}

const miles = (m: number) => (m / 1609.344).toFixed(1);
const mins = (s: number) => `${Math.max(1, Math.round(s / 60))} min`;
function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

// Reopening the app mid-trip goes straight back to it.
if (trip) openDrive();
