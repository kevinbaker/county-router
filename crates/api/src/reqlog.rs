//! Request log for debugging: one JSON line per lookup or route, written to the file in
//! `REQUEST_LOG` (off when unset or empty).
//!
//! Each line has the time, a short request ID (also sent to the browser, which shows it
//! on the route page and print sheet, so a report can point at the exact request), the
//! visitor's address, how long it took, what was asked and what was answered. Parcel
//! outlines and route geometry are left out to keep lines small.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use axum::http::HeaderMap;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tracing::warn;

pub struct RequestLog {
    path: Option<PathBuf>,
    counter: AtomicU64,
}

/// One request being handled: its ID, when it started and who sent it.
pub struct Entry {
    pub id: String,
    started: Instant,
    client: String,
}

impl RequestLog {
    pub fn new(path: &str) -> Self {
        let path = (!path.is_empty()).then(|| PathBuf::from(path));
        if let Some(p) = &path {
            tracing::info!("logging requests to {}", p.display());
        }
        Self {
            path,
            counter: AtomicU64::new(0),
        }
    }

    /// Starts an entry. The client is the first `X-Forwarded-For` address (Caddy sets it
    /// to the real visitor), else the connection's peer.
    pub fn start(&self, headers: &HeaderMap, peer: SocketAddr) -> Entry {
        let client = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| peer.ip().to_string());
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let now = jiff::Timestamp::now().as_nanosecond() as u64;
        // Short and unique enough to find one request in the log.
        let id = format!(
            "{:08x}",
            (now ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15)) as u32
        );
        Entry {
            id,
            started: Instant::now(),
            client,
        }
    }

    /// Appends the finished entry. Reopens the file each time so log rotation needs no
    /// restart; failures are reported but never fail the request.
    pub async fn write(
        &self,
        entry: &Entry,
        endpoint: &str,
        status: u16,
        request: Value,
        response: Value,
    ) {
        let Some(path) = &self.path else { return };
        let line = json!({
            "ts": jiff::Timestamp::now().to_string(),
            "id": entry.id,
            "client": entry.client,
            "endpoint": endpoint,
            "status": status,
            "ms": entry.started.elapsed().as_millis() as u64,
            "request": request,
            "response": response,
        });
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await?;
            file.write_all(format!("{line}\n").as_bytes()).await
        }
        .await;
        if let Err(err) = result {
            warn!("request log {}: {err}", path.display());
        }
    }
}

/// A geocode result for the log: everything but the parcel outline.
pub fn without_parcel(mut v: Value) -> Value {
    if let Some(list) = v.as_array_mut() {
        for item in list {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("parcel");
            }
        }
    }
    v
}

/// A route for the log: everything but the drawn line.
pub fn without_geometry(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("geometry");
    }
    v
}
