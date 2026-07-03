//! HTTP route handlers.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::engine::DB;

use super::dashboard;

/// Shared application state.
#[derive(Clone)]
struct AppState {
    db: DB,
    start_time: Instant,
}

/// Build the Axum router with all routes.
pub fn routes(db: DB) -> Router {
    let state = AppState {
        db,
        start_time: Instant::now(),
    };

    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/metrics", get(metrics))
        .route("/api/stats", get(stats))
        .route("/api/get/{key}", get(get_key))
        .route("/api/put", post(put_key))
        .route("/api/delete/{key}", post(delete_key))
        .route("/api/compact", post(compact))
        .with_state(Arc::new(state))
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
}

#[derive(Serialize)]
struct MetricsResponse {
    writes: u64,
    reads: u64,
    deletes: u64,
    compactions: u64,
    flushes: u64,
}

#[derive(Serialize)]
struct StatsResponse {
    sst_count: usize,
    memtable_size: usize,
}

#[derive(Serialize)]
struct KvResponse {
    found: bool,
    key: String,
    value: Option<String>,
    value_hex: Option<String>,
}

#[derive(Deserialize)]
struct PutRequest {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct ActionResponse {
    success: bool,
    message: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    let m = state.db.metrics();
    let uptime = state.start_time.elapsed().as_secs();
    Html(dashboard::render(uptime, &m))
}

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.start_time.elapsed().as_secs(),
    })
}

async fn metrics(State(state): State<Arc<AppState>>) -> Json<MetricsResponse> {
    let m = state.db.metrics();
    Json(MetricsResponse {
        writes: m.writes,
        reads: m.reads,
        deletes: m.deletes,
        compactions: m.compactions,
        flushes: m.flushes,
    })
}

async fn stats() -> Json<StatsResponse> {
    Json(StatsResponse {
        sst_count: 0,
        memtable_size: 0,
    })
}

async fn get_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<KvResponse>, StatusCode> {
    match state.db.get(key.as_bytes()) {
        Ok(Some(val)) => {
            let hex_str = val.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
            let utf8_str = String::from_utf8_lossy(&val).to_string();
            Ok(Json(KvResponse {
                found: true,
                key,
                value: Some(utf8_str),
                value_hex: Some(hex_str),
            }))
        }
        Ok(None) => Ok(Json(KvResponse {
            found: false,
            key,
            value: None,
            value_hex: None,
        })),
        Err(e) => {
            eprintln!("get error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn put_key(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PutRequest>,
) -> Result<Json<ActionResponse>, StatusCode> {
    match state.db.put(req.key.as_bytes(), req.value.as_bytes()) {
        Ok(()) => Ok(Json(ActionResponse {
            success: true,
            message: format!("OK: wrote {} bytes to '{}'", req.value.len(), req.key),
        })),
        Err(e) => {
            eprintln!("put error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    match state.db.delete(key.as_bytes()) {
        Ok(()) => Ok(Json(ActionResponse {
            success: true,
            message: format!("OK: deleted '{}'", key),
        })),
        Err(e) => {
            eprintln!("delete error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn compact(State(_state): State<Arc<AppState>>) -> Json<ActionResponse> {
    // Compaction is triggered automatically; this endpoint is a placeholder
    // for manual trigger (requires exposing compact on DB).
    Json(ActionResponse {
        success: true,
        message: "compaction is triggered automatically on flush".to_string(),
    })
}
