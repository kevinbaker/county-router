//! Collin Central Appraisal District parcels, looked up by property ID or site address,
//! plus the county's 911 address points for the building on a parcel.
//!
//! Lookups use the local copy that `county-dataprep cad` downloads (`data/out/cad.sqlite`)
//! and fall back to the live public ArcGIS services for anything missing from it, such
//! as a parcel created since the last download. CAD describes its data as public domain.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use county_core::{
    LatLon,
    cad::{ADDRESS_POINTS_URL, PARCELS_URL, ParcelRecord},
};
use geo::{
    BoundingRect, Centroid, ClosestPoint, Contains, Distance, Haversine, MultiPolygon, Point, Rect,
};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use serde_json::Value;
use tracing::warn;

use crate::street;

/// Live-query fields; the same as the download's, minus the object ID.
const LIVE_PARCEL_FIELDS: &str =
    "PROP_ID,situsBldgNum,situsStreetPrefix,situsStreetName,situsStreetSuffix,situsCity,situsZip";
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
    /// Every piece of land under this property ID.
    pub shape: MultiPolygon<f64>,
    /// The lot outline as GeoJSON, for drawing.
    pub geojson: Value,
}

impl Parcel {
    /// Builds a parcel from its address record and the GeoJSON of each of its pieces.
    fn new(record: ParcelRecord, pieces: &[Value]) -> Option<Self> {
        let mut polygons = Vec::new();
        for piece in pieces {
            let Ok(g) = serde_json::from_value::<geojson::Geometry>(piece.clone()) else {
                continue;
            };
            match geo::Geometry::<f64>::try_from(g) {
                Ok(geo::Geometry::Polygon(p)) => polygons.push(p),
                Ok(geo::Geometry::MultiPolygon(mp)) => polygons.extend(mp),
                _ => {}
            }
        }
        if polygons.is_empty() {
            return None;
        }
        let shape = MultiPolygon::new(polygons);
        let geojson =
            serde_json::to_value(geojson::Geometry::new(geojson::GeometryValue::from(&shape)))
                .ok()?;
        Some(Self {
            prop_id: record.prop_id,
            number: record.number,
            street: record.street,
            situs: record.situs,
            shape,
            geojson,
        })
    }

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

/// Merges records that share a property ID (a property split into pieces) into parcels.
fn group(rows: impl IntoIterator<Item = ParcelRecord>) -> Vec<Parcel> {
    let mut order = Vec::new();
    let mut by_id: HashMap<i64, (ParcelRecord, Vec<Value>)> = HashMap::new();
    for r in rows {
        by_id
            .entry(r.prop_id)
            .or_insert_with(|| {
                order.push(r.prop_id);
                (r.clone(), Vec::new())
            })
            .1
            .push(r.geometry);
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .filter_map(|(record, pieces)| Parcel::new(record, &pieces))
        .collect()
}

pub struct Cad {
    http: reqwest::Client,
    /// The downloaded copy; reopened per lookup so a fresh download is picked up.
    local: PathBuf,
    parcels: Mutex<HashMap<i64, Arc<Parcel>>>,
}

impl Cad {
    pub fn new(http: reqwest::Client, local: PathBuf) -> Self {
        if local.exists() {
            tracing::info!("CAD parcels from {}", local.display());
        } else {
            warn!(
                "{} not found; querying CAD live (run `county-dataprep cad`)",
                local.display()
            );
        }
        Self {
            http,
            local,
            parcels: Mutex::new(HashMap::new()),
        }
    }

    fn open_local(&self) -> Option<Connection> {
        if !self.local.exists() {
            return None;
        }
        Connection::open_with_flags(&self.local, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .inspect_err(|err| warn!("opening {}: {err}", self.local.display()))
            .ok()
    }

    /// Fetches the parcels for these property IDs, reusing cached ones. IDs with no
    /// parcel are absent from the result.
    pub async fn by_ids(&self, ids: &[i64]) -> Result<HashMap<i64, Arc<Parcel>>> {
        let mut missing: Vec<i64> = {
            let cache = self.parcels.lock().unwrap();
            ids.iter()
                .copied()
                .filter(|id| !cache.contains_key(id))
                .collect()
        };
        missing.sort_unstable();
        missing.dedup();

        if let Some(db) = self.open_local() {
            let found = local_by_ids(&db, &missing)?;
            missing.retain(|id| !found.iter().any(|p| p.prop_id == *id));
            self.remember(found);
        }
        for chunk in missing.chunks(ID_CHUNK) {
            let list: Vec<String> = chunk.iter().map(i64::to_string).collect();
            let found = self
                .live(&format!("PROP_ID IN ({})", list.join(",")))
                .await?;
            self.remember(found);
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
        let mut best = match self.open_local() {
            Some(db) => addr.best(local_by_number(&db, &addr.number, addr.zip.as_deref())?),
            None => None,
        };
        if best.is_none() {
            let mut filter = format!("situsBldgNum='{}'", addr.number);
            if let Some(zip) = &addr.zip {
                filter.push_str(&format!(" AND situsZip='{zip}'"));
            }
            best = addr.best(self.live(&filter).await?);
        }
        let Some(parcel) = best else {
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
        let number: String = parcel.number.chars().filter(char::is_ascii_digit).collect();
        if number.is_empty() {
            return None;
        }
        let candidates = match self.open_local() {
            Some(db) => local_points(&db, &number, bbox)
                .inspect_err(|err| warn!("local address points: {err:#}"))
                .ok()?,
            None => self.live_points(&number, bbox).await?,
        };
        candidates.into_iter().find_map(|(name, at)| {
            (street::same_street(&name, &parcel.street) && parcel.covers(at)).then_some(at)
        })
    }

    fn remember(&self, parcels: Vec<Parcel>) {
        let mut cache = self.parcels.lock().unwrap();
        for p in parcels {
            cache.insert(p.prop_id, Arc::new(p));
        }
    }

    async fn live(&self, filter: &str) -> Result<Vec<Parcel>> {
        let body: Value = self
            .http
            .get(PARCELS_URL)
            .query(&[
                ("where", filter),
                ("outFields", LIVE_PARCEL_FIELDS),
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
        Ok(group(
            features.iter().filter_map(ParcelRecord::from_feature),
        ))
    }

    async fn live_points(&self, number: &str, bbox: Rect) -> Option<Vec<(String, LatLon)>> {
        let envelope = format!(
            "{},{},{},{}",
            bbox.min().x,
            bbox.min().y,
            bbox.max().x,
            bbox.max().y
        );
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
        Some(
            body["features"]
                .as_array()?
                .iter()
                .filter_map(|f| {
                    let c = f["geometry"]["coordinates"].as_array()?;
                    Some((
                        f["properties"]["St_FullNam"].as_str()?.to_string(),
                        LatLon {
                            lon: c.first()?.as_f64()?,
                            lat: c.get(1)?.as_f64()?,
                        },
                    ))
                })
                .collect(),
        )
    }
}

const LOCAL_PARCEL_COLUMNS: &str = "p.prop_id, p.number, p.street, p.zip, p.situs, s.geometry FROM parcels p JOIN parcel_shapes s USING (prop_id)";

fn local_by_ids(db: &Connection, ids: &[i64]) -> Result<Vec<Parcel>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("SELECT {LOCAL_PARCEL_COLUMNS} WHERE p.prop_id IN ({marks})");
        out.extend(local_query(
            db,
            &sql,
            chunk.iter().map(|id| id.to_string()),
        )?);
    }
    Ok(out)
}

fn local_by_number(db: &Connection, number: &str, zip: Option<&str>) -> Result<Vec<Parcel>> {
    match zip {
        Some(zip) => local_query(
            db,
            &format!("SELECT {LOCAL_PARCEL_COLUMNS} WHERE p.number = ?1 AND p.zip = ?2"),
            [number.to_string(), zip.to_string()],
        ),
        None => local_query(
            db,
            &format!("SELECT {LOCAL_PARCEL_COLUMNS} WHERE p.number = ?1"),
            [number.to_string()],
        ),
    }
}

fn local_query(
    db: &Connection,
    sql: &str,
    args: impl IntoIterator<Item = String>,
) -> Result<Vec<Parcel>> {
    let mut stmt = db.prepare_cached(sql)?;
    let rows = stmt.query_map(params_from_iter(args), |r| {
        let geometry: String = r.get(5)?;
        Ok(ParcelRecord {
            prop_id: r.get(0)?,
            number: r.get(1)?,
            street: r.get(2)?,
            zip: r.get(3)?,
            situs: r.get(4)?,
            geometry: serde_json::from_str(&geometry).unwrap_or(Value::Null),
        })
    })?;
    Ok(group(rows.collect::<rusqlite::Result<Vec<_>>>()?))
}

fn local_points(db: &Connection, number: &str, bbox: Rect) -> Result<Vec<(String, LatLon)>> {
    let mut stmt = db.prepare_cached(
        "SELECT street, lon, lat FROM address_points
         WHERE number = ?1 AND lon BETWEEN ?2 AND ?3 AND lat BETWEEN ?4 AND ?5",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            number,
            bbox.min().x,
            bbox.max().x,
            bbox.min().y,
            bbox.max().y
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                LatLon {
                    lon: r.get(1)?,
                    lat: r.get(2)?,
                },
            ))
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

    /// The candidate whose street matches best: full street (with direction) over core name.
    fn best(&self, candidates: Vec<Parcel>) -> Option<Parcel> {
        let mut best: Option<(u8, Parcel)> = None;
        for p in candidates {
            if let Some(score) = self.street_match(&p.street)
                && best.as_ref().is_none_or(|(s, _)| score > *s)
            {
                best = Some((score, p));
            }
        }
        best.map(|(_, p)| p)
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

    fn piece(prop_id: i64, x: f64) -> ParcelRecord {
        ParcelRecord {
            prop_id,
            number: "10".into(),
            street: "PARKER RD".into(),
            zip: "75098".into(),
            situs: "10 PARKER RD, WYLIE, TX 75098".into(),
            geometry: serde_json::json!({"type": "Polygon",
                "coordinates": [[[x, 33.0], [x + 0.001, 33.0], [x + 0.001, 33.001], [x, 33.0]]]}),
        }
    }

    #[test]
    fn merges_pieces_of_one_property() {
        let parcels = group([piece(7, -96.5), piece(8, -96.4), piece(7, -96.3)]);
        assert_eq!(parcels.len(), 2);
        assert_eq!(parcels[0].prop_id, 7);
        assert_eq!(parcels[0].shape.0.len(), 2);
        assert_eq!(parcels[0].geojson["type"], "MultiPolygon");
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
