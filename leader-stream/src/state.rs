use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{broadcast, RwLock};

use leader_stream::{render_docs, render_index};

use crate::config::Config;
use crate::constants::{API_FALLBACK_SLOT_MS, BROADCAST_BUFFER};
use crate::models::{
    BasePayload, CachedPayload, CurrentSlotPayload, LeaderCache, NextLeadersPayload, NodesCache,
    TrackSchedule,
};
use crate::rpc::RpcClient;
use crate::util::now_ms;

#[derive(Clone)]
pub(crate) enum StreamEvent {
    Payload(BasePayload),
    Error(String),
    Shutdown,
}

#[derive(Clone)]
struct SlotInformerUpdate {
    slot: u64,
    timestamp_ms: u64,
}

pub(crate) struct AppState {
    pub(crate) sender: broadcast::Sender<StreamEvent>,
    pub(crate) latest: Arc<RwLock<Option<BasePayload>>>,
    slot_informer: Arc<RwLock<Option<SlotInformerUpdate>>>,
    pub(crate) leader_cache: Arc<RwLock<LeaderCache>>,
    pub(crate) nodes_cache: Arc<RwLock<NodesCache>>,
    pub(crate) track_cache: Arc<RwLock<HashMap<String, TrackSchedule>>>,
    pub(crate) current_slot_cache: Arc<RwLock<Option<CachedPayload<CurrentSlotPayload>>>>,
    pub(crate) next_leaders_cache: Arc<RwLock<HashMap<usize, CachedPayload<NextLeadersPayload>>>>,
    pub(crate) initial_html: Arc<RwLock<Bytes>>,
    pub(crate) docs_html: Bytes,
    pub(crate) leader_stream_url: String,
    pub(crate) cache_bust: String,
    pub(crate) rpc: RpcClient,
    pub(crate) config: Config,
    pub(crate) track_subscribers: AtomicUsize,
    pub(crate) leader_lookahead: AtomicU64,
    pub(crate) slot_ms_estimate: AtomicU64,
}

impl AppState {
    pub(crate) fn new(config: Config, rpc: RpcClient, leader_stream_url: String) -> Arc<Self> {
        let (sender, _) = broadcast::channel(BROADCAST_BUFFER);
        let cache_bust = now_ms().to_string();
        let base_html = render_index(&leader_stream_url, &cache_bust, None);
        let docs_html = render_docs(&cache_bust);
        Arc::new(Self {
            sender,
            latest: Arc::new(RwLock::new(None)),
            slot_informer: Arc::new(RwLock::new(None)),
            leader_cache: Arc::new(RwLock::new(LeaderCache::default())),
            nodes_cache: Arc::new(RwLock::new(NodesCache::default())),
            track_cache: Arc::new(RwLock::new(HashMap::new())),
            current_slot_cache: Arc::new(RwLock::new(None)),
            next_leaders_cache: Arc::new(RwLock::new(HashMap::new())),
            initial_html: Arc::new(RwLock::new(Bytes::from(base_html))),
            docs_html: Bytes::from(docs_html),
            leader_stream_url,
            cache_bust,
            rpc,
            leader_lookahead: AtomicU64::new(config.leader_lookahead as u64),
            track_subscribers: AtomicUsize::new(0),
            slot_ms_estimate: AtomicU64::new(API_FALLBACK_SLOT_MS),
            config,
        })
    }

    pub(crate) fn on_track_subscribe(&self) {
        let prev = self.track_subscribers.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            self.set_leader_lookahead(self.config.track_lookahead as u64);
        }
    }

    pub(crate) fn on_track_unsubscribe(&self) {
        let prev = self.track_subscribers.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            self.set_leader_lookahead(self.config.leader_lookahead as u64);
        }
    }

    pub(crate) fn set_leader_lookahead(&self, lookahead: u64) {
        let prev = self.leader_lookahead.swap(lookahead, Ordering::SeqCst);
        if prev == lookahead {
            return;
        }
        let cache = Arc::clone(&self.leader_cache);
        tokio::spawn(async move {
            let mut cache = cache.write().await;
            cache.reset();
        });
    }

    pub(crate) fn broadcast_payload(&self, payload: BasePayload) {
        let _ = self.sender.send(StreamEvent::Payload(payload));
    }

    pub(crate) fn broadcast_error(&self, message: impl Into<String>) {
        let _ = self.sender.send(StreamEvent::Error(message.into()));
    }

    pub(crate) fn broadcast_shutdown(&self) {
        let _ = self.sender.send(StreamEvent::Shutdown);
    }

    pub(crate) async fn record_slot_update(&self, slot: u64, timestamp_ms: u64) {
        let mut cache = self.slot_informer.write().await;
        let prev = cache.as_ref().cloned();
        if let Some(prev) = prev.as_ref() {
            if slot <= prev.slot {
                return;
            }
        }
        *cache = Some(SlotInformerUpdate { slot, timestamp_ms });

        if let Some(prev) = prev.as_ref() {
            let slot_delta = slot.saturating_sub(prev.slot);
            if slot_delta > 0 && timestamp_ms > prev.timestamp_ms {
                let elapsed_ms = timestamp_ms.saturating_sub(prev.timestamp_ms);
                let estimate = elapsed_ms / slot_delta;
                if estimate > 0 {
                    let prev_estimate = self.slot_ms_estimate.load(Ordering::SeqCst);
                    let blended = if prev_estimate == 0 {
                        estimate
                    } else {
                        (prev_estimate.saturating_mul(3) + estimate) / 4
                    };
                    self.slot_ms_estimate.store(blended, Ordering::SeqCst);
                }
            }
        }

        let payload = CurrentSlotPayload {
            current_slot: slot,
            ts: now_ms(),
        };
        let mut slot_cache = self.current_slot_cache.write().await;
        *slot_cache = Some(CachedPayload {
            ts_ms: payload.ts,
            payload,
        });
    }

    pub(crate) async fn latest_slot_hint(&self) -> Option<u64> {
        if let Some(slot) = self
            .slot_informer
            .read()
            .await
            .as_ref()
            .map(|update| update.slot)
        {
            return Some(slot);
        }
        self.latest
            .read()
            .await
            .as_ref()
            .map(|payload| payload.slot)
    }
}
