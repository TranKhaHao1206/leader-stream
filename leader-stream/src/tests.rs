use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::config::Config;
use crate::constants::DEFAULT_STATIC_DIR;
use crate::models::{CachedPayload, CurrentSlotPayload, NodeInfo};
use crate::rpc::RpcClient;
use crate::server::build_router;
use crate::state::AppState;

fn test_config() -> Config {
    Config {
        rpc_url: "http://127.0.0.1:1".to_string(),
        rpc_x_token: None,
        ws_url: "ws://127.0.0.1:1".to_string(),
        ws_x_token: None,
        port: 0,
        request_timeout: Duration::from_millis(200),
        node_cache_ttl: Duration::from_millis(1000),
        heartbeat: Duration::from_millis(1000),
        ws_ping_interval: Duration::from_millis(1000),
        leader_lookahead: 100,
        track_lookahead: 200,
    }
}

fn test_state() -> Arc<AppState> {
    let config = test_config();
    let rpc = RpcClient::new(
        config.rpc_url.clone(),
        config.rpc_x_token.clone(),
        config.request_timeout,
    )
    .expect("rpc client");
    AppState::new(config, rpc, "/api/leader-stream".to_string())
}

fn test_app(state: Arc<AppState>) -> axum::Router {
    build_router(state, DEFAULT_STATIC_DIR.to_string())
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = test_app(test_state());
    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .expect("health response");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("health body")
        .to_bytes();
    assert_eq!(body.as_ref(), b"ok");
}

#[tokio::test]
async fn current_slot_returns_cached_payload() {
    let state = test_state();
    let payload = CurrentSlotPayload {
        current_slot: 4242,
        ts: 123,
    };
    {
        let mut cache = state.current_slot_cache.write().await;
        *cache = Some(CachedPayload {
            ts_ms: payload.ts,
            payload: payload.clone(),
        });
    }

    let app = test_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/current-slot")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("current slot response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );

    let body = response
        .into_body()
        .collect()
        .await
        .expect("current slot body")
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).expect("current slot json");
    assert_eq!(value["currentSlot"], 4242);
    assert_eq!(value["ts"], 123);
}

#[tokio::test]
async fn next_leaders_returns_leaders_payload() {
    let state = test_state();
    let leaders = vec!["leader-1", "leader-2", "leader-3"];
    {
        let mut cache = state.leader_cache.write().await;
        cache.start_slot = Some(100);
        cache.leaders = leaders.iter().map(|value| (*value).to_string()).collect();
    }
    {
        let mut cache = state.nodes_cache.write().await;
        cache.nodes_by_pubkey.insert(
            "leader-1".to_string(),
            NodeInfo {
                tpu: Some("127.0.0.1:8001".to_string()),
            },
        );
        cache.nodes_by_pubkey.insert(
            "leader-2".to_string(),
            NodeInfo {
                tpu: Some("http://192.168.1.5:9001".to_string()),
            },
        );
    }

    let app = test_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/next-leaders?limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("next leaders response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );

    let body = response
        .into_body()
        .collect()
        .await
        .expect("next leaders body")
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).expect("next leaders json");
    assert_eq!(value["currentSlot"], 100);
    assert_eq!(value["limit"], 2);
    let rows = value["leaders"].as_array().expect("leaders array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["slot"], 100);
    assert_eq!(rows[0]["leader"], "leader-1");
    assert_eq!(rows[0]["ip"], "127.0.0.1");
    assert_eq!(rows[0]["port"], "8001");
}

#[tokio::test]
async fn leader_stream_sets_event_stream_headers() {
    let app = test_app(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/leader-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("leader stream response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("text/event-stream"))
            .unwrap_or(false)
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache, no-transform")
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok()),
        Some("keep-alive")
    );
    assert_eq!(
        response
            .headers()
            .get("x-accel-buffering")
            .and_then(|value| value.to_str().ok()),
        Some("no")
    );

    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_millis(200), body.frame())
        .await
        .expect("sse frame timeout")
        .expect("sse frame missing")
        .expect("sse frame error");
    let data = match frame.into_data() {
        Ok(data) => data,
        Err(_) => panic!("expected data frame"),
    };
    let text = String::from_utf8_lossy(data.as_ref());
    assert!(text.contains("stream-open"));
}
