//! Address to coordinates.
//!
//! Census Geocoder first: it matches rural house numbers (FM roads, state highways) that
//! OpenStreetMap lacks. Nominatim second: it finds landmarks and streets, but in most of
//! the county only to street level, so those matches are flagged for the user to confirm.

use std::{collections::HashMap, sync::Mutex};

use county_core::{Boundary, LatLon};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use tracing::warn;

const CENSUS_URL: &str = "https://geocoding.geo.census.gov/geocoder/locations/onelineaddress";
const CONCURRENCY: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Matched to a house number.
    High,
    /// Matched to a street or place only; the user should check the pin.
    Low,
    /// Not found.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeocodeResult {
    pub input: String,
    pub confidence: Confidence,
    pub location: Option<LatLon>,
    pub matched: Option<String>,
    pub source: Option<&'static str>,
    pub in_county: bool,
}

pub struct Geocoder {
    http: reqwest::Client,
    nominatim_url: String,
    boundary: Boundary,
    cache: Mutex<HashMap<String, GeocodeResult>>,
}

impl Geocoder {
    pub fn new(http: reqwest::Client, nominatim_url: String, boundary: Boundary) -> Self {
        Self {
            http,
            nominatim_url,
            boundary,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Geocodes addresses in order, a few at a time.
    pub async fn geocode_all(&self, addresses: &[String]) -> Vec<GeocodeResult> {
        // Owned strings: a stream over borrowed items trips axum's `Send` handler bound.
        stream::iter(addresses.to_vec())
            .map(|a| async move { self.geocode(&a).await })
            .buffered(CONCURRENCY)
            .collect()
            .await
    }

    pub async fn geocode(&self, input: &str) -> GeocodeResult {
        let key = normalize(input);
        if let Some(hit) = self.cache.lock().unwrap().get(&key) {
            return hit.clone();
        }

        let found = match self.census(input).await {
            Some(m) => Some(m),
            None => self.nominatim(input).await,
        };
        let result = match found {
            Some((location, matched, source, confidence)) => GeocodeResult {
                input: input.to_string(),
                confidence,
                location: Some(location),
                matched: Some(matched),
                source: Some(source),
                in_county: self.boundary.contains(location),
            },
            None => GeocodeResult {
                input: input.to_string(),
                confidence: Confidence::None,
                location: None,
                matched: None,
                source: None,
                in_county: false,
            },
        };
        self.cache.lock().unwrap().insert(key, result.clone());
        result
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
    fn normalize_ignores_case_and_spacing() {
        assert_eq!(
            normalize("  1500 PARKER RD   Wylie, TX "),
            "1500 parker rd wylie, tx"
        );
    }
}
