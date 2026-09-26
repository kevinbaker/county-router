//! Stop ordering (VROOM) and road geometry (OSRM).

use anyhow::{Context, Result, bail};
use county_core::LatLon;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct SolveRequest {
    pub start: LatLon,
    pub stops: Vec<LatLon>,
    /// Return to the start after the last stop.
    #[serde(default = "yes")]
    pub round_trip: bool,
    /// Time spent at each stop.
    #[serde(default)]
    pub dwell_minutes: f64,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct SolveResponse {
    /// Indexes into the request's `stops`, in driving order.
    pub order: Vec<usize>,
    /// One leg per drive: start to first stop, between stops, and back if round trip.
    pub legs: Vec<Leg>,
    pub total_distance_m: f64,
    pub total_drive_s: f64,
    /// Drive time plus dwell time.
    pub total_s: f64,
    /// Stops VROOM could not fit, as indexes into `stops`.
    pub unassigned: Vec<usize>,
    /// GeoJSON LineString of the whole route.
    pub geometry: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Leg {
    pub distance_m: f64,
    pub duration_s: f64,
}

pub struct Router {
    http: reqwest::Client,
    osrm_url: String,
    vroom_url: String,
}

impl Router {
    pub fn new(http: reqwest::Client, osrm_url: String, vroom_url: String) -> Self {
        Self {
            http,
            osrm_url,
            vroom_url,
        }
    }

    pub async fn solve(&self, req: &SolveRequest) -> Result<SolveResponse> {
        if req.stops.is_empty() {
            bail!("no stops to route");
        }
        let dwell_s = (req.dwell_minutes * 60.0).round().max(0.0) as u64;
        let (order, unassigned) = self.order(req, dwell_s).await?;

        let mut waypoints = vec![req.start];
        waypoints.extend(order.iter().map(|&i| req.stops[i]));
        if req.round_trip {
            waypoints.push(req.start);
        }
        let route = self.route(&waypoints).await?;

        let total_drive_s: f64 = route.legs.iter().map(|l| l.duration_s).sum();
        Ok(SolveResponse {
            total_distance_m: route.legs.iter().map(|l| l.distance_m).sum(),
            total_s: total_drive_s + (dwell_s * order.len() as u64) as f64,
            total_drive_s,
            order,
            unassigned,
            legs: route.legs,
            geometry: route.geometry,
        })
    }

    /// Asks VROOM for the fastest stop order. VROOM gets travel times from OSRM itself.
    async fn order(&self, req: &SolveRequest, dwell_s: u64) -> Result<(Vec<usize>, Vec<usize>)> {
        let start = [req.start.lon, req.start.lat];
        let mut vehicle = json!({ "id": 0, "profile": "car", "start": start });
        if req.round_trip {
            vehicle["end"] = json!(start);
        }
        let jobs: Vec<_> = req
            .stops
            .iter()
            .enumerate()
            .map(|(i, s)| json!({ "id": i, "location": [s.lon, s.lat], "service": dwell_s }))
            .collect();
        let body = json!({ "vehicles": [vehicle], "jobs": jobs });

        let resp: VroomResponse = self
            .http
            .post(format!("{}/", self.vroom_url))
            .json(&body)
            .send()
            .await
            .context("calling VROOM")?
            .error_for_status()
            .context("VROOM rejected the request")?
            .json()
            .await
            .context("reading VROOM response")?;
        if resp.code != 0 {
            bail!(
                "VROOM error {}: {}",
                resp.code,
                resp.error.unwrap_or_default()
            );
        }
        let order = resp
            .routes
            .first()
            .map(|r| {
                r.steps
                    .iter()
                    .filter(|s| s.kind == "job")
                    .filter_map(|s| s.id)
                    .collect()
            })
            .unwrap_or_default();
        let unassigned = resp.unassigned.iter().map(|u| u.id).collect();
        Ok((order, unassigned))
    }

    /// Road geometry and per-leg distance and time through waypoints in the given order.
    async fn route(&self, waypoints: &[LatLon]) -> Result<Route> {
        let coords: Vec<String> = waypoints
            .iter()
            .map(|p| format!("{:.6},{:.6}", p.lon, p.lat))
            .collect();
        let url = format!(
            "{}/route/v1/driving/{}?overview=full&geometries=geojson",
            self.osrm_url,
            coords.join(";")
        );
        let resp: OsrmRouteResponse = self
            .http
            .get(url)
            .send()
            .await
            .context("calling OSRM")?
            .json()
            .await
            .context("reading OSRM response")?;
        if resp.code != "Ok" {
            bail!("OSRM {}: {}", resp.code, resp.message.unwrap_or_default());
        }
        let route = resp
            .routes
            .into_iter()
            .next()
            .context("OSRM returned no route")?;
        Ok(Route {
            legs: route
                .legs
                .into_iter()
                .map(|l| Leg {
                    distance_m: l.distance,
                    duration_s: l.duration,
                })
                .collect(),
            geometry: route.geometry,
        })
    }
}

struct Route {
    legs: Vec<Leg>,
    geometry: serde_json::Value,
}

#[derive(Deserialize)]
struct VroomResponse {
    code: i64,
    error: Option<String>,
    #[serde(default)]
    routes: Vec<VroomRoute>,
    #[serde(default)]
    unassigned: Vec<VroomUnassigned>,
}

#[derive(Deserialize)]
struct VroomRoute {
    steps: Vec<VroomStep>,
}

#[derive(Deserialize)]
struct VroomStep {
    #[serde(rename = "type")]
    kind: String,
    id: Option<usize>,
}

#[derive(Deserialize)]
struct VroomUnassigned {
    id: usize,
}

#[derive(Deserialize)]
struct OsrmRouteResponse {
    code: String,
    message: Option<String>,
    #[serde(default)]
    routes: Vec<OsrmRoute>,
}

#[derive(Deserialize)]
struct OsrmRoute {
    legs: Vec<OsrmLeg>,
    geometry: serde_json::Value,
}

#[derive(Deserialize)]
struct OsrmLeg {
    distance: f64,
    duration: f64,
}
