//! Collin Central Appraisal District parcels, looked up by property ID or site address,
//! plus the county's 911 address points for the building on a parcel.
//!
//! Both are public ArcGIS feature services, queried live and cached in memory. CAD
//! describes its data as public domain.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use county_core::LatLon;
use geo::{
    BoundingRect, Centroid, ClosestPoint, Contains, Distance, Haversine, MultiPolygon, Point,
};
use serde_json::Value;
use tracing::warn;

use crate::street;

const PARCELS_URL: &str = "https://services2.arcgis.com/uXyoacYrZTPTKD3R/ArcGIS/rest/services/CCAD_Parcel_Feature_Set/FeatureServer/4/query";
const ADDRESS_POINTS_URL: &str = "https://services1.arcgis.com/fdWXd5OobWR1E3er/arcgis/rest/services/AddressPoints_1Spatial/FeatureServer/0/query";
const PARCEL_FIELDS: &str = "PROP_ID,situsBldgNum,situsStreetPrefix,situsStreetName,situsStreetSuffix,situsCity,situsZip,ownerName";
/// Property IDs per `IN (...)` query.
const ID_CHUNK: usize = 100;
/// A 911 point this close outside a parcel still counts as that parcel's building.
const POINT_TOLERANCE_M: f64 = 20.0;

#[derive(Debug)]
pub struct Parcel {
    pub prop_id: i64,
    /// House number, e.g. "100".
    pub number: String,
    /// Street as CAD writes it, e.g. "N FOURTH ST".
    pub street: String,
    /// Full site address, e.g. "100 N FOURTH ST, PRINCETON, TX 75407".
    pub situs: String,
    pub owner: Option<String>,
    pub shape: MultiPolygon<f64>,
    /// The lot outline as GeoJSON, for drawing.
    pub geojson: Value,
}

impl Parcel {
    pub fn centroid(&self) -> Option<LatLon> {
        self.shape.centroid().map(|c| LatLon {
            lat: c.y(),
            lon: c.x(),
        })
    }

    /// Whether `p` is on the lot or within a few metres of its edge.
    pub fn covers(&self, p: LatLon) -> bool {
        let point = Point::new(p.lon, p.lat);
        if self.shape.contains(&point) {
            return true;
        }
        match self.shape.closest_point(&point) {
            geo::Closest::Intersection(c) | geo::Closest::SinglePoint(c) => {
                Haversine.distance(point, c) <= POINT_TOLERANCE_M
            }
            geo::Closest::Indeterminate => false,
        }
    }
}

pub struct Cad {
    http: reqwest::Client,
    parcels: Mutex<HashMap<i64, Arc<Parcel>>>,
}

impl Cad {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            parcels: Mutex::new(HashMap::new()),
        }
    }

    /// Fetches the parcels for these property IDs, reusing cached ones. IDs with no
    /// parcel are absent from the result.
    pub async fn by_ids(&self, ids: &[i64]) -> Result<HashMap<i64, Arc<Parcel>>> {
        let missing: Vec<i64> = {
            let cache = self.parcels.lock().unwrap();
            let mut m: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|id| !cache.contains_key(id))
                .collect();
            m.sort_unstable();
            m.dedup();
            m
        };
        for chunk in missing.chunks(ID_CHUNK) {
            let list: Vec<String> = chunk.iter().map(i64::to_string).collect();
            let found = self
                .query(&format!("PROP_ID IN ({})", list.join(",")))
                .await?;
            let mut cache = self.parcels.lock().unwrap();
            for p in found {
                cache.insert(p.prop_id, Arc::new(p));
            }
        }
        let cache = self.parcels.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| cache.get(id).map(|p| (*id, p.clone())))
            .collect())
    }

    /// Finds the parcel whose site address matches a typed address such as
    /// "1200 PARKER RD ST PAUL TX 75098" or "100 N 4th St, Princeton".
    pub async fn by_address(&self, query: &str) -> Result<Option<Arc<Parcel>>> {
        let Some(addr) = TypedAddress::parse(query) else {
            return Ok(None);
        };
        let mut filter = format!("situsBldgNum='{}'", addr.number);
        if let Some(zip) = &addr.zip {
            filter.push_str(&format!(" AND situsZip='{zip}'"));
        }
        let candidates = self.query(&filter).await?;

        // Prefer a match on the full street (with direction) over one on its core name.
        let mut best: Option<(u8, Parcel)> = None;
        for p in candidates {
            if let Some(score) = addr.street_match(&p.street)
                && best.as_ref().is_none_or(|(s, _)| score > *s)
            {
                best = Some((score, p));
            }
        }
        let Some((_, parcel)) = best else {
            return Ok(None);
        };
        let parcel = Arc::new(parcel);
        self.parcels
            .lock()
            .unwrap()
            .insert(parcel.prop_id, parcel.clone());
        Ok(Some(parcel))
    }

    /// The county 911 address point for the building on this parcel, if there is one.
    pub async fn building_point(&self, parcel: &Parcel) -> Option<LatLon> {
        let bbox = parcel.shape.bounding_rect()?;
        let envelope = format!(
            "{},{},{},{}",
            bbox.min().x,
            bbox.min().y,
            bbox.max().x,
            bbox.max().y
        );
        let number: String = parcel.number.chars().filter(char::is_ascii_digit).collect();
        if number.is_empty() {
            return None;
        }
        let resp = self
            .http
            .get(ADDRESS_POINTS_URL)
            .query(&[
                ("where", format!("Add_Number={number}").as_str()),
                ("geometry", &envelope),
                ("geometryType", "esriGeometryEnvelope"),
                ("inSR", "4326"),
                ("spatialRel", "esriSpatialRelIntersects"),
                ("outFields", "St_FullNam"),
                ("outSR", "4326"),
                ("f", "geojson"),
            ])
            .send()
            .await
            .and_then(|r| r.error_for_status());
        let body: Value = match resp {
            Ok(r) => r.json().await.ok()?,
            Err(err) => {
                warn!("county address points: {err}");
                return None;
            }
        };
        body["features"].as_array()?.iter().find_map(|f| {
            let name = f["properties"]["St_FullNam"].as_str()?;
            let c = f["geometry"]["coordinates"].as_array()?;
            let at = LatLon {
                lon: c.first()?.as_f64()?,
                lat: c.get(1)?.as_f64()?,
            };
            (street::same_street(name, &parcel.street) && parcel.covers(at)).then_some(at)
        })
    }

    async fn query(&self, filter: &str) -> Result<Vec<Parcel>> {
        let body: Value = self
            .http
            .get(PARCELS_URL)
            .query(&[
                ("where", filter),
                ("outFields", PARCEL_FIELDS),
                ("outSR", "4326"),
                ("f", "geojson"),
            ])
            .send()
            .await
            .context("querying CAD parcels")?
            .error_for_status()
            .context("CAD parcel service")?
            .json()
            .await
            .context("reading CAD parcels")?;
        if let Some(err) = body.get("error") {
            anyhow::bail!("CAD parcel service: {err}");
        }
        let features = body["features"].as_array().cloned().unwrap_or_default();
        Ok(features.iter().filter_map(parse_parcel).collect())
    }
}

fn parse_parcel(f: &Value) -> Option<Parcel> {
    let p = &f["properties"];
    let text = |k: &str| {
        p[k].as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let prop_id = p["PROP_ID"].as_i64()?;
    let number = text("situsBldgNum").unwrap_or_default();
    let street = [
        text("situsStreetPrefix"),
        text("situsStreetName"),
        text("situsStreetSuffix"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let place = [text("situsCity"), Some("TX".to_string())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    let situs = format!(
        "{} {}, {} {}",
        number,
        street,
        place,
        text("situsZip").unwrap_or_default()
    )
    .trim()
    .to_string();

    let geometry: geojson::Geometry = serde_json::from_value(f["geometry"].clone()).ok()?;
    let shape = match geo::Geometry::<f64>::try_from(geometry).ok()? {
        geo::Geometry::Polygon(poly) => MultiPolygon::new(vec![poly]),
        geo::Geometry::MultiPolygon(mp) => mp,
        _ => return None,
    };
    Some(Parcel {
        prop_id,
        number,
        street,
        situs,
        owner: text("ownerName"),
        shape,
        geojson: f["geometry"].clone(),
    })
}

/// A typed street address split into house number, the words after it, and ZIP.
struct TypedAddress {
    number: String,
    /// Normalized words after the house number (street, then city, state...).
    rest: Vec<String>,
    zip: Option<String>,
}

impl TypedAddress {
    fn parse(query: &str) -> Option<Self> {
        let words: Vec<&str> = query
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|w| !w.is_empty())
            .collect();
        let (first, rest) = words.split_first()?;
        if !first.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let zip = words
            .last()
            .filter(|w| w.len() == 5 && w.chars().all(|c| c.is_ascii_digit()) && words.len() > 2)
            .map(|w| w.to_string());
        let rest = street::words(&rest.join(" "));
        Some(Self {
            number: first.to_string(),
            rest,
            zip,
        })
    }

    /// 2 when the typed address starts with the full street ("N 4TH ST"), 1 when it
    /// starts with the street's core name ("4TH ST", "4TH"), otherwise no match.
    fn street_match(&self, cad_street: &str) -> Option<u8> {
        let full = street::words(cad_street);
        if !full.is_empty() && self.rest.starts_with(&full) {
            return Some(2);
        }
        let core = street::core(cad_street);
        let typed = street::strip_leading_direction(&self.rest);
        (!core.is_empty() && typed.starts_with(&core)).then_some(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(q: &str) -> TypedAddress {
        TypedAddress::parse(q).unwrap()
    }

    #[test]
    fn parses_number_and_zip() {
        let a = typed("1200 PARKER RD ST PAUL, TX 75098");
        assert_eq!(a.number, "1200");
        assert_eq!(a.zip.as_deref(), Some("75098"));
        assert!(TypedAddress::parse("Parker Rd, Wylie").is_none());
    }

    #[test]
    fn matches_cad_streets() {
        assert_eq!(
            typed("100 N FOURTH ST PRINCETON TX 75407").street_match("N FOURTH ST"),
            Some(2)
        );
        assert_eq!(
            typed("100 N 4th Street, Princeton").street_match("N FOURTH ST"),
            Some(2)
        );
        assert_eq!(
            typed("100 4th St, Princeton").street_match("N FOURTH ST"),
            Some(1)
        );
        assert_eq!(
            typed("1200 PARKER RD ST PAUL TX 75098").street_match("PARKER RD"),
            Some(2)
        );
        assert_eq!(
            typed("10000 S STATE HWY 78 LAVON").street_match("S STATE HWY 78"),
            Some(2)
        );
        assert_eq!(
            typed("200 W PRINCETON DR").street_match("PEACHTREE LN"),
            None
        );
    }
}
