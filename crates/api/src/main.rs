//! HTTP API for the Collin County route planner.

mod access;
mod cad;
mod geocode;
mod reqlog;
mod routing;
mod street;
mod tour;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use county_core::Boundary;
use serde::{Deserialize, Serialize};
use tower_governor::{
    GovernorLayer, governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor,
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info};

use crate::{geocode::Geocoder, reqlog::RequestLog, routing::SolveRequest};

struct Config {
    bind: String,
    osrm_url: String,
    vroom_url: String,
    nominatim_url: String,
    boundary_path: String,
    region_path: String,
    build_info_path: String,
    web_dir: String,
    cad_db: String,
    tiles_path: String,
    request_log: String,
}

impl Config {
    fn from_env() -> Self {
        let var =
            |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
        Self {
            bind: var("BIND", "127.0.0.1:8000"),
            osrm_url: var("OSRM_URL", "http://127.0.0.1:5000"),
            vroom_url: var("VROOM_URL", "http://127.0.0.1:3000"),
            nominatim_url: var("NOMINATIM_URL", "http://127.0.0.1:8080"),
            boundary_path: var("BOUNDARY_PATH", "data/boundary/collin.geojson"),
            region_path: var("REGION_PATH", "data/boundary/region.geojson"),
            build_info_path: var("BUILD_INFO_PATH", "data/out/BUILD_INFO"),
            web_dir: var("WEB_DIR", "web/dist"),
            cad_db: var("CAD_DB", "data/out/cad.sqlite"),
            tiles_path: var("TILES_PATH", "data/out/region.pmtiles"),
            request_log: var("REQUEST_LOG", ""),
        }
    }
}

struct AppState {
    config: Config,
    http: reqwest::Client,
    geocoder: Geocoder,
    router: Arc<routing::Router>,
    /// The county outline as loaded, for drawing on the map.
    boundary_geojson: String,
    /// The routable region's outline, for drawing on the map.
    region_geojson: String,
    requests: RequestLog,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    let boundary_geojson = std::fs::read_to_string(&config.boundary_path)
        .with_context(|| format!("reading county boundary from {}", config.boundary_path))?;
    let boundary = Boundary::from_geojson_str(&boundary_geojson)
        .with_context(|| format!("parsing county boundary from {}", config.boundary_path))?;
    let region_geojson = std::fs::read_to_string(&config.region_path)
        .with_context(|| format!("reading region boundary from {}", config.region_path))?;
    let region = Boundary::from_geojson_str(&region_geojson)
        .with_context(|| format!("parsing region boundary from {}", config.region_path))?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let bind = config.bind.clone();
    let web_dir = config.web_dir.clone();
    let tiles_path = config.tiles_path.clone();
    let config_request_log = config.request_log.clone();
    let router = Arc::new(routing::Router::new(
        http.clone(),
        config.osrm_url.clone(),
        config.vroom_url.clone(),
    ));
    let state = Arc::new(AppState {
        geocoder: Geocoder::new(
            http.clone(),
            config.nominatim_url.clone(),
            boundary,
            region,
            router.clone(),
            config.cad_db.clone().into(),
        ),
        router,
        config,
        http,
        boundary_geojson,
        region_geojson,
        requests: RequestLog::new(&config_request_log),
    });

    // Per-visitor limit on the endpoints that do real work. Caddy sets X-Forwarded-For
    // to the actual client address (it ignores one sent by the client), so the key
    // can't be spoofed while the API is only reachable through Caddy.
    let limits = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(env_u64("RATE_LIMIT_REFILL_MS", 2000))
        .burst_size(env_u64("RATE_LIMIT_BURST", 20) as u32)
        .finish()
        .context("rate limit settings")?;
    let limiter = limits.limiter().clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            limiter.retain_recent();
        }
    });
    let limited = Router::new()
        .route("/geocode", post(geocode))
        .route("/solve", post(solve))
        .layer(GovernorLayer::new(limits));
    let api = Router::new()
        .route("/health", get(health))
        .route("/boundary", get(get_boundary))
        .route("/region", get(get_region))
        .merge(limited);
    // The built frontend, when present; in development Vite serves it and proxies /api.
    let app = Router::new()
        .nest("/api", api)
        // Only the tile file itself: the same directory holds the CAD database.
        // ServeFile answers the range requests the map makes for individual tiles.
        .route_service("/tiles/region.pmtiles", ServeFile::new(tiles_path))
        .fallback_service(ServeDir::new(web_dir))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    info!("listening on {bind}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    data_build: Option<String>,
    services: Vec<ServiceHealth>,
}

#[derive(Serialize)]
struct ServiceHealth {
    name: &'static str,
    ok: bool,
    detail: String,
}

/// Reports whether each backing service answers, plus the date of the loaded data build.
async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Health>) {
    let c = &state.config;
    // A point in downtown McKinney; OSRM must be able to snap it to a road.
    let osrm = format!("{}/nearest/v1/driving/-96.6153,33.1976", c.osrm_url);
    let vroom = format!("{}/health", c.vroom_url);
    let nominatim = format!("{}/status?format=json", c.nominatim_url);

    let (osrm, vroom, nominatim) = tokio::join!(
        check(&state.http, "osrm", &osrm),
        check(&state.http, "vroom", &vroom),
        check(&state.http, "nominatim", &nominatim),
    );
    let services = vec![osrm, vroom, nominatim];
    let ok = services.iter().all(|s| s.ok);
    let data_build = std::fs::read_to_string(&c.build_info_path)
        .ok()
        .map(|s| s.trim().to_string());

    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Health {
            ok,
            data_build,
            services,
        }),
    )
}

async fn check(http: &reqwest::Client, name: &'static str, url: &str) -> ServiceHealth {
    match http.get(url).send().await {
        Ok(resp) if resp.status().is_success() => ServiceHealth {
            name,
            ok: true,
            detail: resp.status().to_string(),
        },
        Ok(resp) => ServiceHealth {
            name,
            ok: false,
            detail: resp.status().to_string(),
        },
        Err(err) => ServiceHealth {
            name,
            ok: false,
            detail: err.to_string(),
        },
    }
}

async fn get_boundary(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/geo+json")],
        state.boundary_geojson.clone(),
    )
}

async fn get_region(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/geo+json")],
        state.region_geojson.clone(),
    )
}

/// An upstream failure, reported to the client as 502 with a message.
struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error!("{:#}", self.0);
        let body = Json(serde_json::json!({ "error": format!("{:#}", self.0) }));
        (StatusCode::BAD_GATEWAY, body).into_response()
    }
}

#[derive(Deserialize)]
struct GeocodeRequest {
    addresses: Vec<String>,
}

/// Most stops in one request, matching the UI.
const MAX_STOPS: usize = 250;

fn too_many(n: usize) -> Response {
    let body = Json(serde_json::json!({
        "error": format!("{n} stops; the limit is {MAX_STOPS}")
    }));
    (StatusCode::PAYLOAD_TOO_LARGE, body).into_response()
}

async fn geocode(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<GeocodeRequest>,
) -> Response {
    let entry = state.requests.start(&headers, peer);
    let request = serde_json::json!({ "addresses": req.addresses });
    let (status, logged, response) = if req.addresses.len() > MAX_STOPS {
        let r = too_many(req.addresses.len());
        (r.status(), serde_json::json!("too many stops"), r)
    } else {
        let results = state.geocoder.geocode_all(&req.addresses).await;
        let value = serde_json::to_value(&results).unwrap_or_default();
        (
            StatusCode::OK,
            reqlog::without_parcel(value),
            Json(results).into_response(),
        )
    };
    state
        .requests
        .write(&entry, "geocode", status.as_u16(), request, logged)
        .await;
    with_request_id(response, &entry.id)
}

async fn solve(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SolveRequest>,
) -> Response {
    let entry = state.requests.start(&headers, peer);
    let request = serde_json::to_value(&req).unwrap_or_default();
    let (status, logged, response) = if req.stops.len() > MAX_STOPS {
        let r = too_many(req.stops.len());
        (r.status(), serde_json::json!("too many stops"), r)
    } else {
        match state.router.solve(&req).await {
            Ok(route) => {
                let mut value = serde_json::to_value(&route).unwrap_or_default();
                // Shown on the route page, so a problem report can name the request.
                value["request_id"] = serde_json::json!(entry.id);
                (
                    StatusCode::OK,
                    reqlog::without_geometry(value.clone()),
                    Json(value).into_response(),
                )
            }
            Err(err) => {
                let message = format!("{err:#}");
                let r = ApiError(err).into_response();
                (r.status(), serde_json::json!({ "error": message }), r)
            }
        }
    };
    state
        .requests
        .write(&entry, "solve", status.as_u16(), request, logged)
        .await;
    with_request_id(response, &entry.id)
}

fn with_request_id(mut response: Response, id: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(id) {
        response.headers_mut().insert("x-request-id", v);
    }
    response
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
