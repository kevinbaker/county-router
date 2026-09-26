//! Downloads CAD parcels and county 911 address points into a local SQLite file.
//!
//! Both come from public ArcGIS layers that return at most 2,000 features per request.
//! The download pages through them by object ID, one request at a time with a pause
//! in between, so it stays gentle on the county's servers.

use std::{
    path::Path,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use county_core::cad::{
    ADDRESS_POINT_FIELDS, ADDRESS_POINTS_URL, AddressPointRecord, PARCEL_FIELDS, PARCELS_URL,
    ParcelRecord, SCHEMA,
};
use rusqlite::{Connection, params};
use serde_json::Value;

const USER_AGENT: &str = "county-router/0.1 (Collin County route planner data build)";
const RETRIES: u32 = 5;
/// A download must reach this share of the layer's reported count to replace the old copy.
const MIN_COMPLETE: f64 = 0.98;

pub struct Options {
    pub page_size: u32,
    pub delay: Duration,
    /// Stop after this many pages per layer (for testing).
    pub max_pages: Option<u32>,
}

pub fn download(out: &Path, opts: &Options) -> Result<()> {
    let http = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(120))
        .build()?;

    let tmp = out.with_extension("sqlite.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut db = Connection::open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    db.execute_batch(SCHEMA)?;

    let started = Instant::now();
    let parcels = Layer {
        name: "parcels",
        url: PARCELS_URL,
        fields: PARCEL_FIELDS,
        id_field: "OBJECTID",
    };
    let expected_parcels = parcels.count(&http)?;
    let got_parcels =
        parcels.download(&http, &mut db, opts, expected_parcels, |tx, features| {
            let mut parcel = tx.prepare_cached(
                "INSERT OR IGNORE INTO parcels (prop_id, number, street, zip, situs)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut shape =
                tx.prepare_cached("INSERT INTO parcel_shapes (prop_id, geometry) VALUES (?1, ?2)")?;
            let mut n = 0;
            for p in features.iter().filter_map(ParcelRecord::from_feature) {
                parcel.execute(params![p.prop_id, p.number, p.street, p.zip, p.situs])?;
                shape.execute(params![p.prop_id, p.geometry.to_string()])?;
                n += 1;
            }
            Ok(n)
        })?;

    let points = Layer {
        name: "address points",
        url: ADDRESS_POINTS_URL,
        fields: ADDRESS_POINT_FIELDS,
        id_field: "FID",
    };
    let expected_points = points.count(&http)?;
    let got_points = points.download(&http, &mut db, opts, expected_points, |tx, features| {
        let mut insert = tx.prepare_cached(
            "INSERT OR REPLACE INTO address_points (id, number, street, lon, lat)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut n = 0;
        for a in features.iter().filter_map(AddressPointRecord::from_feature) {
            insert.execute(params![a.id, a.number, a.street, a.lon, a.lat])?;
            n += 1;
        }
        Ok(n)
    })?;

    if opts.max_pages.is_none() {
        for (name, got, expected) in [
            ("parcels", got_parcels, expected_parcels),
            ("address points", got_points, expected_points),
        ] {
            if (got as f64) < expected as f64 * MIN_COMPLETE {
                bail!("only {got} of {expected} {name} downloaded; keeping the previous copy");
            }
        }
    }

    let now = std::process::Command::new("date")
        .args(["-u", "+%FT%TZ"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    db.execute_batch("ANALYZE;")?;
    {
        let mut meta = db.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        meta.execute(params!["downloaded_at", now])?;
        let distinct: i64 = db.query_row("SELECT count(*) FROM parcels", [], |r| r.get(0))?;
        meta.execute(params!["parcels", distinct.to_string()])?;
        meta.execute(params!["parcel_shapes", got_parcels.to_string()])?;
        meta.execute(params!["address_points", got_points.to_string()])?;
    }
    db.close().map_err(|(_, e)| e)?;
    std::fs::rename(&tmp, out).with_context(|| format!("replacing {}", out.display()))?;
    println!(
        "wrote {}: {got_parcels} parcels, {got_points} address points in {:.0?}",
        out.display(),
        started.elapsed()
    );
    Ok(())
}

struct Layer {
    name: &'static str,
    url: &'static str,
    fields: &'static str,
    id_field: &'static str,
}

impl Layer {
    fn count(&self, http: &reqwest::blocking::Client) -> Result<u64> {
        let body = self.get(
            http,
            &[("where", "1=1"), ("returnCountOnly", "true"), ("f", "json")],
        )?;
        body["count"]
            .as_u64()
            .with_context(|| format!("{}: no count in {body}", self.name))
    }

    /// Pages through the layer in object-ID order, handing each page to `store` inside
    /// its own transaction. Returns the number of rows stored.
    fn download(
        &self,
        http: &reqwest::blocking::Client,
        db: &mut Connection,
        opts: &Options,
        expected: u64,
        store: impl Fn(&rusqlite::Transaction, &[Value]) -> Result<u64>,
    ) -> Result<u64> {
        let page_size = opts.page_size.to_string();
        let mut last_id: i64 = -1;
        let mut stored = 0;
        let mut page = 0;
        loop {
            let filter = format!("{} > {last_id}", self.id_field);
            let body = self.get(
                http,
                &[
                    ("where", filter.as_str()),
                    ("outFields", self.fields),
                    ("orderByFields", self.id_field),
                    ("resultRecordCount", page_size.as_str()),
                    ("outSR", "4326"),
                    ("geometryPrecision", "6"),
                    ("f", "geojson"),
                ],
            )?;
            let features = body["features"].as_array().cloned().unwrap_or_default();
            if features.is_empty() {
                break;
            }
            let next_id = features
                .iter()
                .filter_map(|f| {
                    f["properties"][self.id_field]
                        .as_i64()
                        .or_else(|| f["id"].as_i64())
                })
                .max()
                .with_context(|| format!("{}: page without {}", self.name, self.id_field))?;
            if next_id <= last_id {
                bail!("{}: object IDs went backwards at {next_id}", self.name);
            }
            last_id = next_id;

            let tx = db.transaction()?;
            stored += store(&tx, &features)?;
            tx.commit()?;
            page += 1;
            println!("{}: {stored} of {expected} (page {page})", self.name);

            if opts.max_pages.is_some_and(|max| page >= max) {
                break;
            }
            sleep(opts.delay);
        }
        Ok(stored)
    }

    fn get(&self, http: &reqwest::blocking::Client, query: &[(&str, &str)]) -> Result<Value> {
        let mut wait = Duration::from_secs(15);
        for attempt in 1..=RETRIES {
            let result = http
                .get(self.url)
                .query(query)
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json::<Value>());
            match result {
                Ok(body) if body.get("error").is_none() => return Ok(body),
                Ok(body) => eprintln!(
                    "{}: attempt {attempt}: service error {}",
                    self.name, body["error"]
                ),
                Err(err) => eprintln!("{}: attempt {attempt}: {err}", self.name),
            }
            if attempt < RETRIES {
                sleep(wait);
                wait *= 2;
            }
        }
        bail!("{}: giving up after {RETRIES} attempts", self.name)
    }
}
