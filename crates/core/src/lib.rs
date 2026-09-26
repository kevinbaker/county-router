//! Types shared by the API and the data-prep tools.

use std::path::Path;

use geo::{BoundingRect, Contains, Geometry, MultiPolygon, Point, Rect};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum BoundaryError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("parsing GeoJSON: {0}")]
    GeoJson(#[from] geojson::Error),
    #[error("GeoJSON contains no polygons")]
    NoPolygons,
}

/// A WGS84 coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// A county outline used to decide whether a point is in the service area.
#[derive(Debug, Clone)]
pub struct Boundary {
    shape: MultiPolygon<f64>,
    bbox: Rect<f64>,
}

impl Boundary {
    pub fn new(shape: MultiPolygon<f64>) -> Result<Self, BoundaryError> {
        let bbox = shape.bounding_rect().ok_or(BoundaryError::NoPolygons)?;
        Ok(Self { shape, bbox })
    }

    /// Loads every Polygon and MultiPolygon in a GeoJSON file (geometry, feature or collection).
    pub fn from_geojson_file(path: impl AsRef<Path>) -> Result<Self, BoundaryError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| BoundaryError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_geojson_str(&text)
    }

    pub fn from_geojson_str(text: &str) -> Result<Self, BoundaryError> {
        let gj: geojson::GeoJson = text.parse()?;
        let collection = geo::GeometryCollection::<f64>::try_from(&gj)?;
        let mut polygons = Vec::new();
        for geometry in collection {
            match geometry {
                Geometry::Polygon(p) => polygons.push(p),
                Geometry::MultiPolygon(mp) => polygons.extend(mp),
                _ => {}
            }
        }
        Self::new(MultiPolygon::new(polygons))
    }

    pub fn shape(&self) -> &MultiPolygon<f64> {
        &self.shape
    }

    pub fn contains(&self, at: LatLon) -> bool {
        let point = Point::new(at.lon, at.lat);
        self.bbox.contains(&point) && self.shape.contains(&point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon",
        "coordinates":[[[-96.8,33.0],[-96.3,33.0],[-96.3,33.4],[-96.8,33.4],[-96.8,33.0]]]}}"#;

    #[test]
    fn contains_inside_and_rejects_outside() {
        let b = Boundary::from_geojson_str(SQUARE).unwrap();
        assert!(b.contains(LatLon {
            lat: 33.2,
            lon: -96.6
        }));
        assert!(!b.contains(LatLon {
            lat: 32.9,
            lon: -96.6
        }));
        assert!(!b.contains(LatLon {
            lat: 33.2,
            lon: -97.0
        }));
    }

    #[test]
    fn rejects_geojson_without_polygons() {
        let point = r#"{"type":"Point","coordinates":[-96.6,33.2]}"#;
        assert!(matches!(
            Boundary::from_geojson_str(point),
            Err(BoundaryError::NoPolygons)
        ));
    }

    #[test]
    fn real_county_outline() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/boundary/collin.geojson"
        );
        let b = Boundary::from_geojson_file(path).unwrap();
        let at = |lat, lon| b.contains(LatLon { lat, lon });
        assert!(at(33.1976, -96.6153), "downtown McKinney");
        assert!(at(33.0198, -96.6989), "Plano");
        assert!(!at(33.2148, -97.1331), "Denton");
        assert!(!at(32.9312, -96.4597), "Rockwall");
    }
}
