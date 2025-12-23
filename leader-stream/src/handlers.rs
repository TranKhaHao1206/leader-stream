use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_stream::stream;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::warn;

use leader_stream::render_index;

use crate::constants::{
    API_CACHE_TTL_MS, API_FALLBACK_SLOT_MS, INITIAL_PAYLOAD_LIMIT, NEXT_LEADERS_DEFAULT_LIMIT,
    NEXT_LEADERS_MAX_LIMIT, NEXT_LEADERS_MIN_LIMIT,
};
use crate::models::{
    BasePayload, CachedPayload, CurrentSlotPayload, LeaderRowPayload, NextLeadersPayload, Payload,
    PayloadNoTrack, TrackSchedule,
};
use crate::state::{AppState, StreamEvent};
use crate::util::{now_ms, parse_tpu_address};

struct TrackGuard {
    state: Arc<AppState>,
    active: bool,
}

impl Drop for TrackGuard {
    fn drop(&mut self) {
        if self.active {
            self.state.on_track_unsubscribe();
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct StreamParams {
    track: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct LeadersParams {
    limit: Option<String>,
}

pub(crate) async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

pub(crate) async fn index_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let html = state.initial_html.read().await.clone();
    let mut response = Response::new(Body::from(html));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

pub(crate) async fn docs_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut response = Response::new(Body::from(state.docs_html.clone()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

pub(crate) async fn current_slot_handler(State(state): State<Arc<AppState>>) -> Response {
    let now = now_ms();
    if let Some(slot) = state.latest_slot_hint().await {
        let payload = CurrentSlotPayload {
            current_slot: slot,
            ts: now,
        };
        return json_response(&payload);
    }
    {
        let cache = state.current_slot_cache.read().await;
        if let Some(entry) = cache.as_ref() {
            return json_response(&entry.payload);
        }
    }

    error_response("slot cache warming; try again shortly".to_string())
}

pub(crate) async fn next_leaders_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LeadersParams>,
) -> Response {
    let limit = clamp_limit(params.limit.as_deref());
    let now = now_ms();
    let latest_slot = state.latest_slot_hint().await;

    {
        let cache = state.next_leaders_cache.read().await;
        if let Some(entry) = cache.get(&limit) {
            if now.saturating_sub(entry.ts_ms) < API_CACHE_TTL_MS
                && (latest_slot.is_none() || latest_slot == Some(entry.payload.current_slot))
            {
                return json_response(&entry.payload);
            }
        }
    }

    let result = build_next_leaders_payload(&state, limit, latest_slot, now).await;

    match result {
        Ok(payload) => {
            {
                let mut cache = state.next_leaders_cache.write().await;
                cache.insert(
                    limit,
                    CachedPayload {
                        ts_ms: now,
                        payload: payload.clone(),
                    },
                );
            }
            json_response(&payload)
        }
        Err(err) => {
            let cache = state.next_leaders_cache.read().await;
            if let Some(entry) = cache.get(&limit) {
                return json_response(&entry.payload);
            }
            error_response(err.to_string())
        }
    }
}

pub(crate) async fn options_handler() -> impl IntoResponse {
    (StatusCode::NO_CONTENT, cors_headers())
}

fn json_response<T: Serialize>(payload: &T) -> Response {
    let body = match serde_json::to_string(payload) {
        Ok(body) => body,
        Err(err) => return error_response(err.to_string()),
    };
    let mut headers = cors_headers();
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    headers.insert(
        "Cache-Control",
        HeaderValue::from_static(
            "public, s-maxage=1, stale-while-revalidate=1, stale-if-error=30",
        ),
    );
    (StatusCode::OK, headers, body).into_response()
}

fn error_response(message: String) -> Response {
    let headers = cors_headers();
    (StatusCode::INTERNAL_SERVER_ERROR, headers, message).into_response()
}

fn clamp_limit(input: Option<&str>) -> usize {
    let value = input
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(NEXT_LEADERS_DEFAULT_LIMIT);
    value.clamp(NEXT_LEADERS_MIN_LIMIT, NEXT_LEADERS_MAX_LIMIT)
}

async fn build_next_leaders_payload(
    state: &AppState,
    limit: usize,
    latest_slot: Option<u64>,
    now: u64,
) -> Result<NextLeadersPayload> {
    let mut current_slot = latest_slot;
    if current_slot.is_none() {
        let cache = state.leader_cache.read().await;
        current_slot = cache.start_slot;
    }
    let mut current_slot = match current_slot {
        Some(slot) => slot,
        None => return Err(anyhow!("leader cache warming; try again shortly")),
    };

    let leaders = {
        let cache = state.leader_cache.read().await;
        let start_slot = match cache.start_slot {
            Some(start_slot) => start_slot,
            None => return Err(anyhow!("leader cache warming; try again shortly")),
        };
        if cache.leaders.is_empty() {
            return Err(anyhow!("leader cache warming; try again shortly"));
        }
        if current_slot < start_slot {
            current_slot = start_slot;
        }
        let offset = current_slot.saturating_sub(start_slot) as usize;
        if offset >= cache.leaders.len() {
            return Err(anyhow!(
                "leader cache missing for current slot; try again shortly"
            ));
        }
        let available = cache.leaders.len() - offset;
        let take = limit.min(available);
        cache.leaders[offset..offset + take].to_vec()
    };

    let nodes_cache = state.nodes_cache.read().await;
    let leader_rows = leaders
        .into_iter()
        .enumerate()
        .map(|(index, leader)| {
            let node = nodes_cache.nodes_by_pubkey.get(&leader);
            let tpu = node.and_then(|node| node.tpu.clone());
            let addr = parse_tpu_address(tpu.as_deref());
            LeaderRowPayload {
                slot: current_slot + index as u64,
                leader,
                tpu,
                ip: addr.ip,
                port: addr.port,
            }
        })
        .collect::<Vec<_>>();

    let slot_ms = state
        .slot_ms_estimate
        .load(Ordering::SeqCst)
        .max(API_FALLBACK_SLOT_MS);

    Ok(NextLeadersPayload {
        current_slot,
        limit,
        leaders: leader_rows,
        slot_ms,
        ts: now,
    })
}

pub(crate) async fn refresh_initial_payload(state: &Arc<AppState>) {
    let limit = INITIAL_PAYLOAD_LIMIT;
    let latest_slot = state.latest_slot_hint().await;
    let now = now_ms();
    let payload = match build_next_leaders_payload(state, limit, latest_slot, now).await {
        Ok(payload) => payload,
        Err(_) => return,
    };
    let payload_json = match serialize_payload_for_html(&payload) {
        Some(json) => json,
        None => return,
    };
    let html = render_index(&state.leader_stream_url, &state.cache_bust, Some(&payload_json));
    {
        let mut cache = state.initial_html.write().await;
        *cache = Bytes::from(html);
    }
    {
        let mut cache = state.next_leaders_cache.write().await;
        cache.insert(
            limit,
            CachedPayload {
                ts_ms: now,
                payload,
            },
        );
    }
}

fn serialize_payload_for_html(payload: &NextLeadersPayload) -> Option<String> {
    let json = serde_json::to_string(payload).ok()?;
    if json.contains("</") {
        Some(json.replace("</", "<\\/"))
    } else {
        Some(json)
    }
}

pub(crate) async fn leader_stream(
    State(state): State<Arc<AppState>>,
    Query(params): Query<StreamParams>,
) -> Response {
    let track = params
        .track
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());

    let track_active = track.is_some();
    if track_active {
        state.on_track_subscribe();
    }

    let guard = TrackGuard {
        state: Arc::clone(&state),
        active: track_active,
    };

    let stream_state = Arc::clone(&state);
    let heartbeat = state.config.heartbeat;

    let stream = stream! {
        let _guard = guard;
        let mut rx = stream_state.sender.subscribe();

        yield Ok::<_, Infallible>(Event::default().comment("stream-open"));

        if let Some(payload) = stream_state.latest.read().await.clone() {
            if let Some(event) = build_payload_event(&stream_state, payload, track.as_deref()).await {
                yield Ok::<_, Infallible>(event);
            }
        }

        loop {
            match rx.recv().await {
                Ok(StreamEvent::Payload(payload)) => {
                    if let Some(event) = build_payload_event(&stream_state, payload, track.as_deref()).await {
                        yield Ok::<_, Infallible>(event);
                    }
                }
                Ok(StreamEvent::Error(message)) => {
                    let json = serde_json::json!({ "message": message }).to_string();
                    yield Ok::<_, Infallible>(Event::default().event("error").data(json));
                }
                Ok(StreamEvent::Shutdown) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    warn!("leader-stream lagged; skipping messages");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(heartbeat).text("heartbeat"));
    let mut response = sse.into_response();
    apply_stream_headers(&mut response);
    response
}

async fn build_payload_event(
    state: &AppState,
    payload: BasePayload,
    track: Option<&str>,
) -> Option<Event> {
    let json = if let Some(track) = track {
        let slots_until_leader = resolve_slots_until_leader(state, track, payload.slot).await;
        serde_json::to_string(&Payload::from_base(payload, slots_until_leader))
    } else {
        serde_json::to_string(&PayloadNoTrack::from_base(payload))
    };

    match json {
        Ok(json) => Some(Event::default().data(json)),
        Err(err) => {
            warn!(?err, "failed to serialize payload");
            None
        }
    }
}

async fn resolve_slots_until_leader(state: &AppState, track: &str, slot: u64) -> Option<u64> {
    {
        let cache = state.leader_cache.read().await;
        if let Some(value) = cache.slots_until_leader(track, slot) {
            return Some(value);
        }
    }

    {
        let cache = state.track_cache.read().await;
        if let Some(entry) = cache.get(track) {
            if entry.covers_slot(slot) {
                return entry.slots_until(slot);
            }
        }
    }

    let epoch_info = match state.rpc.get_epoch_info().await {
        Ok(info) => info,
        Err(err) => {
            warn!(?err, "failed to fetch epoch info");
            return None;
        }
    };

    let epoch_start = epoch_info.absoluteSlot.saturating_sub(epoch_info.slotIndex);
    let slots = match state.rpc.get_leader_schedule(slot, track).await {
        Ok(result) => result.unwrap_or_default(),
        Err(err) => {
            warn!(?err, "failed to fetch leader schedule");
            return None;
        }
    };

    let mut abs_slots: Vec<u64> = slots
        .into_iter()
        .map(|index| epoch_start.saturating_add(index))
        .collect();
    abs_slots.sort_unstable();

    let entry = TrackSchedule {
        epoch_start,
        slots_in_epoch: epoch_info.slotsInEpoch,
        slots: abs_slots,
    };

    {
        let mut cache = state.track_cache.write().await;
        cache.insert(track.to_string(), entry.clone());
    }

    entry.slots_until(slot)
}

fn cors_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
    headers.insert(
        "Access-Control-Allow-Methods",
        HeaderValue::from_static("GET, OPTIONS"),
    );
    headers.insert(
        "Access-Control-Allow-Headers",
        HeaderValue::from_static("Content-Type"),
    );
    headers
}

fn apply_stream_headers(response: &mut Response) {
    let headers = response.headers_mut();
    headers.extend(cors_headers());
    headers.insert(
        "Cache-Control",
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert("Connection", HeaderValue::from_static("keep-alive"));
    headers.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
}
