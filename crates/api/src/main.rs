//! HTTP API for the Collin County route planner.

mod geocode;
mod routing;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use county_core::Boundary;
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tracing::{error, info};

use crate::{
    geocode::{GeocodeResult, Geocoder},
    routing::{SolveRequest, SolveResponse},
};

struct Config {
    bind: String,
    osrm_url: String,
    vroom_url: String,
    nominatim_url: String,
    boundary_path: String,
    build_info_path: String,
    web_dir: String,
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
            build_info_path: var("BUILD_INFO_PATH", "data/out/BUILD_INFO"),
            web_dir: var("WEB_DIR", "web/dist"),
        }
    }
}

struct AppState {
    config: Config,
    http: reqwest::Client,
    geocoder: Geocoder,
    router: routing::Router,
    /// The county outline as loaded, for drawing on the map.
    boundary_geojson: String,
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
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let bind = config.bind.clone();
    let web_dir = config.web_dir.clone();
    let state = Arc::new(AppState {
        geocoder: Geocoder::new(http.clone(), config.nominatim_url.clone(), boundary),
        router: routing::Router::new(
            http.clone(),
            config.osrm_url.clone(),
            config.vroom_url.clone(),
        ),
        config,
        http,
        boundary_geojson,
    });

    let api = Router::new()
        .route("/health", get(health))
        .route("/boundary", get(get_boundary))
        .route("/geocode", post(geocode))
        .route("/solve", post(solve));
    // The built frontend, when present; in development Vite serves it and proxies /api.
    let app = Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(web_dir))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    info!("listening on {bind}");
    axum::serve(listener, app).await?;
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

async fn geocode(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GeocodeRequest>,
) -> Json<Vec<GeocodeResult>> {
    Json(state.geocoder.geocode_all(&req.addresses).await)
}

async fn solve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SolveRequest>,
) -> Result<Json<SolveResponse>, ApiError> {
    state.router.solve(&req).await.map(Json).map_err(ApiError)
}
