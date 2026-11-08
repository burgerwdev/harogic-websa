//! HTTP parsing and responses, route registration and static assets.
//! Device mutation and publication belong to service_commands.
use super::{
    channel_controls, command_error_reply, command_request_id, multichannel, receiver_presets,
    recording, recording_collection, recording_demod, recording_export, recording_read,
    service_commands, service_preflight, service_stream, with_request_id, ws_handler, App,
};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub(super) fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/state", get(api_state))
        .route("/api/schema", get(api_schema))
        .route("/api/config", post(api_config))
        .route("/ws", get(ws_handler))
        .route("/ws/iq", get(multichannel::iq_ws))
        .route("/ws/audio", get(service_stream::audio_ws_handler))
        .route("/ws/decode", get(service_stream::decode_ws_handler))
        .route("/api/sdr/plugins", get(channel_controls::plugins))
        .route(
            "/api/sdr/receivers",
            get(multichannel::snapshot).post(multichannel::command),
        )
        .route(
            "/api/sdr/receivers/preflight",
            post(multichannel::preflight),
        )
        .route(
            "/api/sdr/receiver-presets",
            get(receiver_presets::list).post(receiver_presets::change),
        )
        .route("/api/sdr/recordings/{task}/index", get(recording::index))
        .route(
            "/api/sdr/recordings/{task}/range",
            get(recording_read::range),
        )
        .route(
            "/api/sdr/recordings/{task}/export",
            get(recording_export::export),
        )
        .route(
            "/api/sdr/recordings/{task}/collection",
            get(recording_collection::export),
        )
        .route(
            "/api/sdr/recordings/{task}/demodulate",
            get(recording_demod::export),
        )
        .route(
            "/api/sdr/recordings/{task}/results",
            get(recording::results::export),
        )
        .route(
            "/api/sdr/recordings/{task}/audio",
            get(recording::audio::export),
        )
        .route(
            "/api/sdr/recordings/{task}/audio/flac",
            get(recording::flac::export),
        )
        .route(
            "/api/sdr/recordings/{task}/audio/opus",
            get(recording::opus::export),
        )
        .route(
            "/api/sdr/recordings/{task}/audio/range",
            get(recording::audio_read::export),
        )
        .route("/", get(index))
        .route("/static/dist/{*path}", get(static_files))
        .with_state(app)
}

pub(super) fn browser_access(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin
        .to_str()
        .ok()
        .and_then(|origin| {
            origin
                .strip_prefix("http://")
                .or_else(|| origin.strip_prefix("https://"))
        })
        .is_some_and(|origin_host| origin_host == host)
}

pub(super) fn error_body(msg: &str, code: &str) -> Value {
    json!({"error": msg, "code": code})
}

pub(super) fn state_worker_failed(error: tokio::task::JoinError) -> StatusCode {
    eprintln!("WebSA state worker failed: {error}");
    StatusCode::SERVICE_UNAVAILABLE
}

async fn api_state(State(app): State<Arc<App>>) -> Result<Json<Value>, StatusCode> {
    let worker_app = app.clone();
    let status = tokio::task::spawn_blocking(move || worker_app.state.status_with_revision())
        .await
        .map_err(state_worker_failed)?;
    Ok(Json(app.state.current_status(status.0, status.1).1))
}

async fn api_schema(State(app): State<Arc<App>>) -> Result<Json<Value>, StatusCode> {
    let schema = tokio::task::spawn_blocking(move || app.state.lock().unwrap().schema.clone())
        .await
        .map_err(state_worker_failed)?;
    Ok(Json(schema))
}

pub(super) async fn api_config(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !browser_access(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(data) = serde_json::from_str::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(error_body("bad json", "bad_json")),
        )
            .into_response();
    };
    let cmd = data
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let request_id = command_request_id(&data);
    let admission = match service_preflight::preflight(&app, &cmd, &data).await {
        Ok(admission) => admission,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(with_request_id(
                    command_error_reply(error_body(&error, "bad_parameter"), &cmd),
                    request_id.as_ref(),
                )),
            )
                .into_response()
        }
    };
    match service_commands::execute(app.clone(), admission, cmd.clone(), data).await {
        Ok(Ok(reply)) => {
            let (_, mut status) = app.state.current_status(reply.revision, reply.status);
            status["changed"] = json!(reply.changed);
            status["response_to"] = json!(cmd);
            Json(with_request_id(status, request_id.as_ref())).into_response()
        }
        Ok(Err(error)) => {
            let mut body = with_request_id(
                command_error_reply(error_body(&error.message, error.code), &cmd),
                request_id.as_ref(),
            );
            if error.params.as_object().is_some_and(|p| !p.is_empty()) {
                body["params"] = error.params;
            }
            (StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
        Err(error) => state_worker_failed(error).into_response(),
    }
}

/// Frontend root that holds `dist/`: WEBSA_STATIC, else the repository
/// `frontend/` directory of this build.
fn static_root() -> String {
    std::env::var("WEBSA_STATIC")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../frontend").into())
}

async fn index() -> Response {
    match tokio::fs::read(format!("{}/dist/index.html", static_root())).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], bytes).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "frontend is not built").into_response(),
    }
}

async fn static_files(Path(path): Path<String>) -> Response {
    let root = match tokio::fs::canonicalize(format!("{}/dist", static_root())).await {
        Ok(root) => root,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let full = match tokio::fs::canonicalize(root.join(path)).await {
        Ok(full) if full.starts_with(&root) => full,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let kind = match full.extension().and_then(|ext| ext.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("txt" | "md") => "text/plain; charset=utf-8",
                Some("gz") => "application/gzip",
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("wasm") => "application/wasm",
                Some("json") => "application/json",
                Some("woff2") => "font/woff2",
                Some("png") => "image/png",
                Some("svg") => "image/svg+xml",
                _ => "application/octet-stream",
            };
            (
                [
                    (header::CONTENT_TYPE, kind),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
