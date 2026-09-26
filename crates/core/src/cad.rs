//! Collin CAD parcels and county 911 address points: the public ArcGIS layers they come
//! from, how their features are read, and the local SQLite copy the data build makes.

use serde_json::Value;

/// CAD parcel polygons with site addresses. CAD describes its data as public domain.
pub const PARCELS_URL: &str = "https://services2.arcgis.com/uXyoacYrZTPTKD3R/ArcGIS/rest/services/CCAD_Parcel_Feature_Set/FeatureServer/4/query";
/// Fields read from the parcel layer. Owner names are deliberately not fetched.
pub const PARCEL_FIELDS: &str = "OBJECTID,PROP_ID,situsBldgNum,situsStreetPrefix,situsStreetName,situsStreetSuffix,situsCity,situsZip";

/// County 911 address points: one per building.
pub const ADDRESS_POINTS_URL: &str = "https://services1.arcgis.com/fdWXd5OobWR1E3er/arcgis/rest/services/AddressPoints_1Spatial/FeatureServer/0/query";
pub const ADDRESS_POINT_FIELDS: &str = "FID,Add_Number,St_FullNam";

/// Schema of the local copy (`data/out/cad.sqlite`).
pub const SCHEMA: &str = "
CREATE TABLE parcels (
    prop_id  INTEGER PRIMARY KEY,
    number   TEXT NOT NULL,
    street   TEXT NOT NULL,
    zip      TEXT NOT NULL,
    situs    TEXT NOT NULL
);
CREATE INDEX parcels_number_zip ON parcels (number, zip);

-- A property can be several separate pieces of land, one row each.
CREATE TABLE parcel_shapes (
    prop_id  INTEGER NOT NULL,
    geometry TEXT NOT NULL      -- GeoJSON, WGS84
);
CREATE INDEX parcel_shapes_prop_id ON parcel_shapes (prop_id);

CREATE TABLE address_points (
    id     INTEGER PRIMARY KEY,
    number TEXT NOT NULL,
    street TEXT NOT NULL,
    lon    REAL NOT NULL,
    lat    REAL NOT NULL
);
CREATE INDEX address_points_number ON address_points (number, lon, lat);

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// One CAD parcel as stored: site address parts and the lot outline.
#[derive(Debug, Clone)]
pub struct ParcelRecord {
    pub prop_id: i64,
    /// House number, e.g. "100".
    pub number: String,
    /// Street as CAD writes it, e.g. "N FOURTH ST".
    pub street: String,
    pub zip: String,
    /// Full site address, e.g. "100 N FOURTH ST, PRINCETON, TX 75407".
    pub situs: String,
    /// GeoJSON geometry (Polygon or MultiPolygon), WGS84.
    pub geometry: Value,
}

impl ParcelRecord {
    /// Reads a feature from the parcel layer's `f=geojson` output. A property split into
    /// several pieces comes back as several features with the same `prop_id`.
    pub fn from_feature(f: &Value) -> Option<Self> {
        let p = &f["properties"];
        let text = |k: &str| {
            p[k].as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let prop_id = p["PROP_ID"].as_i64().filter(|&id| id > 0)?;
        let geometry = f.get("geometry").filter(|g| !g.is_null())?.clone();
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
        let zip = text("situsZip").unwrap_or_default();
        let place = [text("situsCity"), Some("TX".to_string())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        let situs = format!("{number} {street}, {place} {zip}")
            .trim()
            .to_string();
        Some(Self {
            prop_id,
            number,
            street,
            zip,
            situs,
            geometry,
        })
    }
}

/// One county 911 address point.
#[derive(Debug, Clone)]
pub struct AddressPointRecord {
    pub id: i64,
    pub number: String,
    /// Full street name, e.g. "S STATE HIGHWAY 78".
    pub street: String,
    pub lon: f64,
    pub lat: f64,
}

impl AddressPointRecord {
    /// Reads a feature from the address point layer's `f=geojson` output.
    pub fn from_feature(f: &Value) -> Option<Self> {
        let p = &f["properties"];
        let c = f["geometry"]["coordinates"].as_array()?;
        let number = match &p["Add_Number"] {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.trim().to_string(),
            _ => return None,
        };
        Some(Self {
            id: p["FID"].as_i64().or_else(|| f["id"].as_i64())?,
            number,
            street: p["St_FullNam"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string(),
            lon: c.first()?.as_f64()?,
            lat: c.get(1)?.as_f64()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_a_parcel_feature() {
        let f = json!({
            "type": "Feature",
            "geometry": {"type": "Polygon", "coordinates": [[[-96.5, 33.1], [-96.4, 33.1], [-96.4, 33.2], [-96.5, 33.1]]]},
            "properties": {"PROP_ID": 1000001, "situsBldgNum": "100", "situsStreetPrefix": "N",
                           "situsStreetName": "FOURTH", "situsStreetSuffix": "ST",
                           "situsCity": "PRINCETON", "situsZip": "75407"}
        });
        let p = ParcelRecord::from_feature(&f).unwrap();
        assert_eq!(p.street, "N FOURTH ST");
        assert_eq!(p.situs, "100 N FOURTH ST, PRINCETON, TX 75407");
    }

    #[test]
    fn reads_an_address_point_feature() {
        let f = json!({
            "type": "Feature", "id": 7,
            "geometry": {"type": "Point", "coordinates": [-96.41, 33.04]},
            "properties": {"FID": 7, "Add_Number": 1021, "St_FullNam": "W FM 6"}
        });
        let a = AddressPointRecord::from_feature(&f).unwrap();
        assert_eq!((a.number.as_str(), a.street.as_str()), ("1021", "W FM 6"));
    }
}
