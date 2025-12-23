use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::http::HeaderValue;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tracing::{error, info, warn};

use crate::constants::{DEFAULT_LEADER_CACHE_REFRESH_MS, DEFAULT_TRACK_LOOKAHEAD};
use crate::handlers::refresh_initial_payload;
use crate::models::{
    BasePayload, CachedPayload, CurrentSlotPayload, LeaderCache, NodeInfo, NodesCache,
};
use crate::rpc::{JsonRpcEnvelope, RpcClient};
use crate::state::AppState;
use crate::util::{now_ms, parse_tpu_address};

pub(crate) async fn run_leader_cache_updater(state: Arc<AppState>) {
    let base_interval = Duration::from_millis(DEFAULT_LEADER_CACHE_REFRESH_MS);
    let mut backoff = base_interval;
    let max_backoff = Duration::from_secs(30);

    loop {
        match refresh_leader_cache(&state).await {
            Ok(()) => {
                backoff = base_interval;
                refresh_initial_payload(&state).await;
                tokio::time::sleep(base_interval).await;
            }
            Err(err) => {
                warn!(?err, "leader cache update failed; retrying");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
        }
    }
}

pub(crate) async fn run_subscriber_metrics(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let subscriber_count = state.sender.receiver_count();
        if subscriber_count == 0 {
            continue;
        }

        let track_subscribers = state.track_subscribers.load(Ordering::SeqCst);
        let leader_lookahead = state.leader_lookahead.load(Ordering::SeqCst);
        let slot_ms_estimate = state.slot_ms_estimate.load(Ordering::SeqCst);

        let (latest_slot, latest_slot_ts) = {
            let latest = state.latest.read().await;
            if let Some(payload) = latest.as_ref() {
                (Some(payload.slot), Some(payload.slotTimestamp))
            } else {
                (None, None)
            }
        };

        let (leader_cache_start, leader_cache_end, leader_cache_len) = {
            let cache = state.leader_cache.read().await;
            (cache.start_slot, cache.end_slot(), cache.leaders.len())
        };

        let now = now_ms();
        let nodes_cache_age_ms = {
            let nodes_cache = state.nodes_cache.read().await;
            if nodes_cache.ts_ms == 0 {
                None
            } else {
                Some(now.saturating_sub(nodes_cache.ts_ms))
            }
        };
        let track_cache_len = { state.track_cache.read().await.len() };
        let current_slot_cache_age_ms = {
            let current_slot_cache = state.current_slot_cache.read().await;
            current_slot_cache
                .as_ref()
                .map(|entry| now.saturating_sub(entry.ts_ms))
        };

        let latest_slot = latest_slot
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let latest_slot_ts = latest_slot_ts
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let leader_cache_start = leader_cache_start
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let leader_cache_end = leader_cache_end
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let nodes_cache_age_ms = nodes_cache_age_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let current_slot_cache_age_ms = current_slot_cache_age_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());

        info!(
            subscribers = subscriber_count,
            track_subscribers,
            leader_lookahead,
            slot_ms_estimate,
            latest_slot = %latest_slot,
            latest_slot_ts = %latest_slot_ts,
            leader_cache_start = %leader_cache_start,
            leader_cache_end = %leader_cache_end,
            leader_cache_len,
            nodes_cache_age_ms = %nodes_cache_age_ms,
            track_cache_len,
            current_slot_cache_age_ms = %current_slot_cache_age_ms,
            "subscriber metrics"
        );
    }
}

async fn refresh_leader_cache(state: &Arc<AppState>) -> Result<()> {
    let lookahead = state.leader_lookahead.load(Ordering::SeqCst) as usize;
    if lookahead == 0 {
        return Ok(());
    }

    let current_slot = match state.latest_slot_hint().await {
        Some(slot) => slot,
        None => state.rpc.get_slot().await?,
    };

    {
        let payload = CurrentSlotPayload {
            current_slot,
            ts: now_ms(),
        };
        let mut slot_cache = state.current_slot_cache.write().await;
        *slot_cache = Some(CachedPayload {
            ts_ms: payload.ts,
            payload,
        });
    }

    if let Err(err) = refresh_nodes_if_needed(state).await {
        warn!(?err, "failed to refresh nodes cache");
    }

    let mut cache = state.leader_cache.write().await;
    ensure_leaders_for_range(&state.rpc, &mut cache, current_slot, current_slot, lookahead).await
}

pub(crate) async fn run_slot_informer(state: Arc<AppState>) {
    match connect_and_informer(Arc::clone(&state)).await {
        Ok(()) => {
            warn!("slot informer ended; exiting to trigger restart");
        }
        Err(err) => {
            state.broadcast_error(err.to_string());
            error!(?err, "slot informer error; exiting to trigger restart");
        }
    }
    std::process::exit(1);
}

async fn connect_and_informer(state: Arc<AppState>) -> Result<()> {
    let mut request = state
        .config
        .ws_url
        .clone()
        .into_client_request()
        .context("invalid websocket URL")?;

    if let Some(token) = &state.config.ws_x_token {
        let header_value = HeaderValue::from_str(token).context("invalid websocket token")?;
        request.headers_mut().insert("x-token", header_value);
    }

    let (ws_stream, _) = connect_async(request)
        .await
        .context("websocket connect failed")?;
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    let subscribe_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "slotsUpdatesSubscribe",
    });

    ws_sink
        .send(Message::Text(subscribe_request.to_string()))
        .await
        .context("subscribe request failed")?;

    info!("connected to slotsUpdatesSubscribe websocket");

    let mut ping_interval = tokio::time::interval(state.config.ws_ping_interval);

    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if let Err(err) = ws_sink.send(Message::Ping(Vec::new())).await {
                    return Err(anyhow!("failed to send ping: {}", err));
                }
            }
            message = ws_stream.next() => {
                let message = match message {
                    Some(message) => message,
                    None => break,
                };
                let message = message.context("websocket stream error")?;
                match message {
                    Message::Text(text) => {
                        let envelope: JsonRpcEnvelope = match serde_json::from_str(&text) {
                            Ok(message) => message,
                            Err(err) => {
                                warn!(?err, "failed to parse websocket message");
                                continue;
                            }
                        };

                        if let Some(error) = envelope.error {
                            return Err(anyhow!("websocket error: {}", error.message));
                        }

                        if envelope.method.as_deref() != Some("slotsUpdatesNotification") {
                            if envelope.id.is_some() && envelope.result.is_some() {
                                info!("slotsUpdatesSubscribe confirmed");
                            }
                            continue;
                        }

                        let params = match envelope.params {
                            Some(params) => params,
                            None => continue,
                        };
                        let _subscription_id = params.subscription;

                        if !is_first_shred_update(&params.result.update_type) {
                            continue;
                        }

                        let slot = params.result.slot;
                        let slot_timestamp = params.result.timestamp.unwrap_or_else(now_ms);
                        state.record_slot_update(slot, slot_timestamp).await;
                        if let Err(err) = process_slot(&state, slot, slot_timestamp).await {
                            state.broadcast_error(err.to_string());
                            warn!(?err, "failed to process slot");
                        }
                    }
                    Message::Ping(payload) => {
                        if let Err(err) = ws_sink.send(Message::Pong(payload)).await {
                            return Err(anyhow!("failed to respond to ping: {}", err));
                        }
                    }
                    Message::Close(frame) => {
                        return Err(anyhow!("websocket closed: {:?}", frame));
                    }
                    _ => {}
                }
            }
        }
    }

    Err(anyhow!("websocket stream ended"))
}

fn is_first_shred_update(update_type: &str) -> bool {
    matches!(update_type, "firstShredReceived" | "firstShred")
}

async fn process_slot(state: &AppState, slot: u64, slot_timestamp: u64) -> Result<()> {
    let last_slot = state
        .latest
        .read()
        .await
        .as_ref()
        .map(|payload| payload.slot);

    if let Some(last_slot) = last_slot {
        if slot <= last_slot {
            return Ok(());
        }
    }

    refresh_nodes_if_needed(state).await?;

    let lookahead = state.leader_lookahead.load(Ordering::SeqCst) as usize;
    {
        let mut cache = state.leader_cache.write().await;
        ensure_leaders_for_range(&state.rpc, &mut cache, slot, slot, lookahead).await?;
    }

    let cache = state.leader_cache.read().await;
    let nodes_cache = state.nodes_cache.read().await;
    if let Some(payload) = build_payload_for_slot(slot, slot_timestamp, &cache, &nodes_cache) {
        {
            let mut latest = state.latest.write().await;
            *latest = Some(payload.clone());
        }
        state.broadcast_payload(payload);
    }

    Ok(())
}

async fn refresh_nodes_if_needed(state: &AppState) -> Result<()> {
    let now = now_ms();
    {
        let cache = state.nodes_cache.read().await;
        if !cache.nodes_by_pubkey.is_empty()
            && now.saturating_sub(cache.ts_ms) < state.config.node_cache_ttl.as_millis() as u64
        {
            return Ok(());
        }
    }

    let nodes = state.rpc.get_cluster_nodes().await?;
    let mut next_map = HashMap::new();
    for node in nodes {
        next_map.insert(
            node.pubkey,
            NodeInfo {
                tpu: node.tpu.clone(),
            },
        );
    }

    let mut cache = state.nodes_cache.write().await;
    cache.nodes_by_pubkey = next_map;
    cache.ts_ms = now;

    Ok(())
}

async fn ensure_leaders_for_range(
    rpc: &RpcClient,
    cache: &mut LeaderCache,
    start_slot: u64,
    end_slot: u64,
    lookahead: usize,
) -> Result<()> {
    let lookahead = lookahead.max(2).min(DEFAULT_TRACK_LOOKAHEAD);
    let range_start = std::cmp::min(start_slot, end_slot);
    let range_end = std::cmp::max(start_slot, end_slot);
    let cache_end = cache.end_slot();
    let refresh_threshold = (lookahead / 4).max(1) as u64;

    let needs_refresh = cache.start_slot.is_none()
        || cache.leaders.is_empty()
        || cache_end.is_none()
        || range_start < cache.start_slot.unwrap()
        || range_end > cache_end.unwrap()
        || cache_end.unwrap().saturating_sub(range_end) < refresh_threshold;

    if !needs_refresh {
        return Ok(());
    }

    let leaders = rpc.get_slot_leaders(range_start, lookahead).await?;
    cache.start_slot = Some(range_start);
    cache.leaders = leaders;

    Ok(())
}

fn build_payload_for_slot(
    slot: u64,
    slot_timestamp: u64,
    cache: &LeaderCache,
    nodes_cache: &NodesCache,
) -> Option<BasePayload> {
    let start_slot = cache.start_slot?;
    if slot < start_slot {
        return None;
    }
    let offset = (slot - start_slot) as usize;
    if offset >= cache.leaders.len() {
        return None;
    }

    let current_validator = cache.leaders.get(offset)?.to_string();
    let current_validator = if current_validator.is_empty() {
        None
    } else {
        Some(current_validator)
    };

    let mut run_length = 0u64;
    let mut next_validator = None;

    if let Some(current) = current_validator.as_ref() {
        for leader in cache.leaders.iter().skip(offset) {
            if leader != current {
                next_validator = Some(leader.clone());
                break;
            }
            run_length += 1;
        }
    }

    let current_node = current_validator
        .as_ref()
        .and_then(|validator| nodes_cache.nodes_by_pubkey.get(validator));
    let next_node = next_validator
        .as_ref()
        .and_then(|validator| nodes_cache.nodes_by_pubkey.get(validator));

    let current_tpu = current_node.and_then(|node| node.tpu.clone());
    let next_tpu = next_node.and_then(|node| node.tpu.clone());

    let current_addr = parse_tpu_address(current_tpu.as_deref());
    let next_addr = parse_tpu_address(next_tpu.as_deref());

    let slots_until_switch = if current_validator.is_some() && next_validator.is_some() {
        Some(run_length)
    } else {
        None
    };

    Some(BasePayload {
        slot,
        slotTimestamp: slot_timestamp,
        currentValidator: current_validator,
        nextValidator: next_validator,
        currentTpu: current_tpu,
        currentIp: current_addr.ip,
        currentPort: current_addr.port,
        nextTpu: next_tpu,
        nextIp: next_addr.ip,
        nextPort: next_addr.port,
        slotsUntilSwitch: slots_until_switch,
    })
}
