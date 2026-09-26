//! Offline data preparation for the route planner. Run by `data/build.sh`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use county_core::Boundary;
use geo::{Buffer, Centroid, MapCoords, MultiPolygon};

#[derive(Parser)]
#[command(about = "Data preparation for the Collin County route planner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Writes the county outline and a buffered copy for clipping the OSM extract.
    Boundary {
        /// County GeoJSON as downloaded from TIGERweb.
        #[arg(long)]
        input: PathBuf,
        /// Buffer distance in kilometres.
        #[arg(long, default_value_t = 5.0)]
        buffer_km: f64,
        /// Output for the unbuffered outline (used for the in-county check).
        #[arg(long)]
        out: PathBuf,
        /// Output for the buffered outline (used by `osmium extract`).
        #[arg(long)]
        out_buffered: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Boundary {
            input,
            buffer_km,
            out,
            out_buffered,
        } => {
            let boundary = Boundary::from_geojson_file(&input)
                .with_context(|| format!("loading {}", input.display()))?;
            let buffered = buffer_meters(boundary.shape(), buffer_km * 1000.0)?;
            write_geojson(&out, boundary.shape())?;
            write_geojson(&out_buffered, &buffered)?;
            println!(
                "wrote {} and {} ({buffer_km} km buffer)",
                out.display(),
                out_buffered.display()
            );
            Ok(())
        }
    }
}

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Buffers a lon/lat shape by a distance in metres.
///
/// Projects to a local equirectangular plane centred on the shape. Across one county
/// (under 1 degree of latitude) the scale error is well under 1%, so a 5 km buffer is
/// accurate to a few tens of metres, which is plenty for clipping a road network.
fn buffer_meters(shape: &MultiPolygon<f64>, meters: f64) -> Result<MultiPolygon<f64>> {
    let center = shape.centroid().context("shape has no centroid")?;
    let (lon0, lat0) = (center.x(), center.y());
    // Metres per degree of latitude, and of longitude at the centre latitude.
    let k = EARTH_RADIUS_M.to_radians();
    let kx = k * lat0.to_radians().cos();

    let projected = shape.map_coords(|c| geo::coord! { x: (c.x - lon0) * kx, y: (c.y - lat0) * k });
    let buffered = projected.buffer(meters);
    Ok(buffered.map_coords(|c| geo::coord! { x: c.x / kx + lon0, y: c.y / k + lat0 }))
}

fn write_geojson(path: &PathBuf, shape: &MultiPolygon<f64>) -> Result<()> {
    let geometry = geojson::Geometry::new(geojson::GeometryValue::from(shape));
    let feature = geojson::Feature {
        geometry: Some(geometry),
        ..Default::default()
    };
    std::fs::write(path, geojson::GeoJson::Feature(feature).to_string())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{Contains, Point, polygon};

    #[test]
    fn buffer_grows_by_roughly_the_requested_distance() {
        // About 1 km square near McKinney.
        let square: MultiPolygon<f64> = polygon![
            (x: -96.62, y: 33.19), (x: -96.61, y: 33.19),
            (x: -96.61, y: 33.20), (x: -96.62, y: 33.20), (x: -96.62, y: 33.19),
        ]
        .into();
        let buffered = buffer_meters(&square, 5000.0).unwrap();
        // 0.04 degrees of latitude is about 4.4 km: inside. 0.05 degrees is about 5.6 km: outside.
        assert!(buffered.contains(&Point::new(-96.615, 33.20 + 0.04)));
        assert!(!buffered.contains(&Point::new(-96.615, 33.20 + 0.05)));
    }
}
