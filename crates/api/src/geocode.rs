//! Stop lookup: a CAD property ID or a typed address to a map point and a route point.
//!
//! Order: the CAD parcel (by property ID, or by matching the typed address to a parcel's
//! site address); then the Census Geocoder, which knows rural house numbers that
//! OpenStreetMap lacks; then Nominatim, which in most of the county matches only to the
//! street, so those stops are flagged for the user to check.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use county_core::{Boundary, LatLon};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::{
    access::{self, Method},
    cad::{Cad, Parcel},
    routing::Router,
};

const CENSUS_URL: &str = "https://geocoding.geo.census.gov/geocoder/locations/onelineaddress";
const CONCURRENCY: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Matched to a house or lot.
    High,
    /// Matched to a street, place or lot centre only; the user should check the pin.
    Low,
    /// Not found.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeocodeResult {
    pub input: String,
    pub confidence: Confidence,
    /// Where to show the stop.
    pub location: Option<LatLon>,
    /// Point on the addressed street to route to, when known.
    pub route_location: Option<LatLon>,
    pub matched: Option<String>,
    pub source: Option<&'static str>,
    /// Inside Collin County.
    pub in_county: bool,
    /// Inside the routable region (Collin and its neighbouring counties).
    pub in_area: bool,
    pub prop_id: Option<i64>,
    /// How the point was placed on a CAD parcel.
    pub method: Option<Method>,
    /// The lot outline as a GeoJSON geometry.
    pub parcel: Option<Value>,
    /// Why nothing was found, when known.
    pub note: Option<String>,
}

impl GeocodeResult {
    fn not_found(input: &str, note: Option<String>) -> Self {
        Self {
            input: input.to_string(),
            confidence: Confidence::None,
            location: None,
            route_location: None,
            matched: None,
            source: None,
            in_county: false,
            in_area: false,
            prop_id: None,
            method: None,
            parcel: None,
            note,
        }
    }
}

pub struct Geocoder {
    http: reqwest::Client,
    nominatim_url: String,
    /// Collin County.
    boundary: Boundary,
    /// Collin and its neighbours: everything the road data covers.
    region: Boundary,
    cad: Cad,
    router: Arc<Router>,
    cache: Mutex<HashMap<String, GeocodeResult>>,
}

impl Geocoder {
    pub fn new(
        http: reqwest::Client,
        nominatim_url: String,
        boundary: Boundary,
        region: Boundary,
        router: Arc<Router>,
        cad_db: std::path::PathBuf,
    ) -> Self {
        Self {
            cad: Cad::new(http.clone(), cad_db),
            http,
            nominatim_url,
            boundary,
            region,
            router,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Looks up stops in order, a few at a time. Property IDs are fetched from CAD in
    /// batches first.
    pub async fn geocode_all(&self, inputs: &[String]) -> Vec<GeocodeResult> {
        let ids: Vec<i64> = inputs.iter().filter_map(|i| property_id(i)).collect();
        let parcels = if ids.is_empty() {
            Some(HashMap::new())
        } else {
            match self.cad.by_ids(&ids).await {
                Ok(found) => Some(found),
                Err(err) => {
                    warn!("CAD lookup by property ID: {err:#}");
                    None
                }
            }
        };
        let parcels = &parcels;
        // Owned strings: a stream over borrowed items trips axum's `Send` handler bound.
        stream::iter(inputs.to_vec())
            .map(|q| async move { self.geocode(&q, parcels.as_ref()).await })
            .buffered(CONCURRENCY)
            .collect()
            .await
    }

    /// `parcels` holds the prefetched property-ID lookups; `None` if CAD was unreachable.
    async fn geocode(
        &self,
        input: &str,
        parcels: Option<&HashMap<i64, Arc<Parcel>>>,
    ) -> GeocodeResult {
        let key = normalize(input);
        if let Some(hit) = self.cache.lock().unwrap().get(&key) {
            return hit.clone();
        }

        let result = if let Some(id) = property_id(input) {
            match parcels {
                Some(found) => match found.get(&id) {
                    Some(p) => self.locate_parcel(input, p).await,
                    None => GeocodeResult::not_found(
                        input,
                        Some(format!("No CAD parcel with property ID {id}")),
                    ),
                },
                // Not cached: CAD may be back on the next try.
                None => {
                    return GeocodeResult::not_found(
                        input,
                        Some("CAD parcel service unavailable".into()),
                    );
                }
            }
        } else {
            let parcel = match self.cad.by_address(input).await {
                Ok(p) => p,
                Err(err) => {
                    warn!("CAD lookup by address: {err:#}");
                    None
                }
            };
            match parcel {
                Some(p) => self.locate_parcel(input, &p).await,
                None => self.street_geocode(input).await,
            }
        };
        self.cache.lock().unwrap().insert(key, result.clone());
        result
    }

    async fn locate_parcel(&self, input: &str, parcel: &Parcel) -> GeocodeResult {
        let building = self.cad.building_point(parcel).await;
        let Some(spot) = access::locate(&self.router, parcel, building).await else {
            return GeocodeResult::not_found(input, Some("CAD parcel has no shape".into()));
        };
        GeocodeResult {
            input: input.to_string(),
            confidence: if spot.method == Method::LotCentre {
                Confidence::Low
            } else {
                Confidence::High
            },
            location: Some(spot.display),
            route_location: spot.route,
            matched: Some(parcel.situs.clone()),
            source: Some("cad"),
            in_county: self.boundary.contains(spot.display),
            in_area: self.region.contains(spot.display),
            prop_id: Some(parcel.prop_id),
            method: Some(spot.method),
            parcel: Some(parcel.geojson.clone()),
            note: None,
        }
    }

    /// Addresses with no CAD parcel: Census, then Nominatim.
    async fn street_geocode(&self, input: &str) -> GeocodeResult {
        let found = match self.census(input).await {
            Some(m) => Some(m),
            None => self.nominatim(input).await,
        };
        match found {
            Some((location, matched, source, confidence)) => GeocodeResult {
                confidence,
                location: Some(location),
                matched: Some(matched),
                source: Some(source),
                in_county: self.boundary.contains(location),
                in_area: self.region.contains(location),
                ..GeocodeResult::not_found(input, None)
            },
            None => GeocodeResult::not_found(input, None),
        }
    }

    async fn census(&self, input: &str) -> Option<(LatLon, String, &'static str, Confidence)> {
        let resp = self
            .http
            .get(CENSUS_URL)
            .query(&[
                ("address", input),
                ("benchmark", "Public_AR_Current"),
                ("format", "json"),
            ])
            .send()
            .await
            .and_then(|r| r.error_for_status());
        let body: CensusResponse = match resp {
            Ok(r) => r.json().await.ok()?,
            Err(err) => {
                warn!("census geocoder: {err}");
                return None;
            }
        };
        let m = body.result.address_matches.into_iter().next()?;
        let at = LatLon {
            lat: m.coordinates.y,
            lon: m.coordinates.x,
        };
        Some((at, m.matched_address, "census", Confidence::High))
    }

    async fn nominatim(&self, input: &str) -> Option<(LatLon, String, &'static str, Confidence)> {
        let url = format!("{}/search", self.nominatim_url);
        let resp = self
            .http
            .get(url)
            .query(&[
                ("q", input),
                ("format", "jsonv2"),
                ("limit", "1"),
                ("addressdetails", "1"),
                ("countrycodes", "us"),
            ])
            .send()
            .await
            .and_then(|r| r.error_for_status());
        let hits: Vec<NominatimHit> = match resp {
            Ok(r) => r.json().await.ok()?,
            Err(err) => {
                warn!("nominatim: {err}");
                return None;
            }
        };
        let hit = hits.into_iter().next()?;
        let at = LatLon {
            lat: hit.lat.parse().ok()?,
            lon: hit.lon.parse().ok()?,
        };
        let confidence = if hit.address.house_number.is_some() {
            Confidence::High
        } else {
            Confidence::Low
        };
        Some((at, hit.display_name, "nominatim", confidence))
    }
}

/// A CAD property ID: digits only, optionally written with a leading `#`.
fn property_id(input: &str) -> Option<i64> {
    let id = input.trim().trim_start_matches('#');
    if id.is_empty() || id.len() > 9 || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}

/// Cache key: case and spacing don't change the answer.
fn normalize(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Deserialize)]
struct CensusResponse {
    result: CensusResult,
}

#[derive(Deserialize)]
struct CensusResult {
    #[serde(rename = "addressMatches")]
    address_matches: Vec<CensusMatch>,
}

#[derive(Deserialize)]
struct CensusMatch {
    #[serde(rename = "matchedAddress")]
    matched_address: String,
    coordinates: CensusCoordinates,
}

#[derive(Deserialize)]
struct CensusCoordinates {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct NominatimHit {
    lat: String,
    lon: String,
    display_name: String,
    #[serde(default)]
    address: NominatimAddress,
}

#[derive(Deserialize, Default)]
struct NominatimAddress {
    house_number: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_property_ids() {
        assert_eq!(property_id("1000001"), Some(1000001));
        assert_eq!(property_id(" #1000002 "), Some(1000002));
        assert_eq!(property_id("100 N FOURTH ST"), None);
        assert_eq!(property_id("75407"), Some(75407));
        assert_eq!(property_id(""), None);
    }

    #[test]
    fn normalize_ignores_case_and_spacing() {
        assert_eq!(
            normalize("  1500 PARKER RD   Wylie, TX "),
            "1500 parker rd wylie, tx"
        );
    }
}
