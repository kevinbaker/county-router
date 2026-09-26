//! HTTP API for the Collin County route planner.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use county_core::Boundary;
use serde::Serialize;
use tracing::info;

struct Config {
    bind: String,
    osrm_url: String,
    vroom_url: String,
    nominatim_url: String,
    boundary_path: String,
    build_info_path: String,
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
        }
    }
}

struct AppState {
    config: Config,
    http: reqwest::Client,
    #[allow(dead_code)] // used by /geocode in Phase 1
    boundary: Boundary,
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
    let boundary = Boundary::from_geojson_file(&config.boundary_path)
        .with_context(|| format!("loading county boundary from {}", config.boundary_path))?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let bind = config.bind.clone();
    let state = Arc::new(AppState {
        config,
        http,
        boundary,
    });

    let app = Router::new()
        .route("/health", get(health))
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
