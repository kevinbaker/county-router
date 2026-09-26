//! Where on a parcel to put a stop: a point to show on the map, and a point on the
//! addressed street to route to.
//!
//! Rural lots can be long strips reaching a back road, so the lot centre is often
//! nearer the wrong street. The route point is always taken on the street named in the
//! site address when that street can be found next to the lot.

use county_core::LatLon;
use futures::{StreamExt, stream};
use geo::{Coord, Densify, Haversine, LineString};
use serde::Serialize;

use crate::{
    cad::Parcel,
    routing::{Router, Snap},
    street,
};

/// Spacing of the points sampled along the lot edge.
const EDGE_STEP_M: f64 = 15.0;
/// Most edge points to snap per parcel.
const MAX_EDGE_POINTS: usize = 80;
/// Road candidates per edge point; the addressed street is often not the very nearest.
const SNAPS_PER_POINT: u32 = 3;
/// Road candidates around a building when looking for its street.
const SNAPS_NEAR_BUILDING: u32 = 10;
/// The addressed street must be within this distance of the building.
const MAX_BUILDING_TO_STREET_M: f64 = 250.0;
const CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The county 911 point for the building.
    Building,
    /// The lot edge facing the addressed street.
    Frontage,
    /// The middle of the lot: the addressed street wasn't found next to it.
    LotCentre,
}

#[derive(Debug, Clone)]
pub struct Access {
    pub display: LatLon,
    /// Point on the addressed street; `None` means route to the nearest road.
    pub route: Option<LatLon>,
    pub method: Method,
}

pub async fn locate(router: &Router, parcel: &Parcel, building: Option<LatLon>) -> Option<Access> {
    let frontage = frontage(router, parcel).await;

    if let Some(b) = building {
        let near_building = router
            .nearest(b, SNAPS_NEAR_BUILDING)
            .await
            .ok()
            .and_then(|snaps| {
                snaps.into_iter().find(|s| {
                    s.distance_m <= MAX_BUILDING_TO_STREET_M
                        && street::same_street(&s.name, &parcel.street)
                })
            });
        return Some(Access {
            display: b,
            route: near_building
                .or(frontage.map(|(_, road)| road))
                .map(|s| s.location),
            method: Method::Building,
        });
    }
    if let Some((edge, road)) = frontage {
        return Some(Access {
            display: edge,
            route: Some(road.location),
            method: Method::Frontage,
        });
    }
    parcel.centroid().map(|c| Access {
        display: c,
        route: None,
        method: Method::LotCentre,
    })
}

/// The lot-edge point closest to the addressed street, and where it meets that street.
async fn frontage(router: &Router, parcel: &Parcel) -> Option<(LatLon, Snap)> {
    let edge = edge_points(parcel);
    let matches: Vec<(LatLon, Snap)> = stream::iter(edge)
        .map(|p| async move {
            let snaps = router.nearest(p, SNAPS_PER_POINT).await.unwrap_or_default();
            snaps
                .into_iter()
                .find(|s| street::same_street(&s.name, &parcel.street))
                .map(|s| (p, s))
        })
        .buffer_unordered(CONCURRENCY)
        .filter_map(|m| async move { m })
        .collect()
        .await;
    matches
        .into_iter()
        .min_by(|a, b| a.1.distance_m.total_cmp(&b.1.distance_m))
}

/// Points every few metres along the outer edges of the lot, thinned to a fixed budget.
fn edge_points(parcel: &Parcel) -> Vec<LatLon> {
    let coords: Vec<Coord> = parcel
        .shape
        .iter()
        .flat_map(|poly| {
            let ring: &LineString = poly.exterior();
            Haversine.densify(ring, EDGE_STEP_M).0
        })
        .collect();
    let step = coords.len().div_ceil(MAX_EDGE_POINTS).max(1);
    coords
        .iter()
        .step_by(step)
        .map(|c| LatLon { lat: c.y, lon: c.x })
        .collect()
}
