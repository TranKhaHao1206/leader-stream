use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderValue, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;
use tracing::info;

use crate::handlers::{
    current_slot_handler, docs_handler, health_handler, index_handler, leader_path_handler,
    leader_stream, map_handler, next_leaders_handler, options_handler,
};
use crate::state::AppState;

pub(crate) fn build_router(state: Arc<AppState>, static_dir: String) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/docs", get(docs_handler))
        .route("/docs.html", get(docs_handler))
        .route("/map", get(map_handler))
        .route("/map.html", get(map_handler))
        .route(
            "/api/current-slot",
            get(current_slot_handler).options(options_handler),
        )
        .route(
            "/api/next-leaders",
            get(next_leaders_handler).options(options_handler),
        )
        .route(
            "/api/leader-path",
            get(leader_path_handler).options(options_handler),
        )
        .route(
            "/api/leader-stream",
            get(leader_stream).options(options_handler),
        )
        .route("/health", get(health_handler))
        .fallback_service(ServeDir::new(static_dir))
        .with_state(state)
        .layer(middleware::from_fn(add_hsts_header))
        .layer(middleware::from_fn(log_request))
}

async fn add_hsts_header(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains; preload"),
    );
    response
}

async fn log_request(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or_else(|| req.uri().path())
        .to_string();
    let remote_ip = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            req.extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0.ip().to_string())
        })
        .unwrap_or_else(|| "-".to_string());
    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    let elapsed = start.elapsed();
    info!(
        remote_ip = %remote_ip,
        method = %method,
        uri = %uri,
        status,
        elapsed_ms = elapsed.as_millis(),
        "request served"
    );
    response
}
