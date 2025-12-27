use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CachedPayload<T> {
    pub(crate) ts_ms: u64,
    pub(crate) payload: T,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CurrentSlotPayload {
    pub(crate) current_slot: u64,
    pub(crate) ts: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NextLeadersPayload {
    pub(crate) current_slot: u64,
    pub(crate) limit: usize,
    pub(crate) leaders: Vec<LeaderRowPayload>,
    pub(crate) slot_ms: u64,
    pub(crate) ts: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct LeaderRowPayload {
    pub(crate) slot: u64,
    pub(crate) leader: String,
    pub(crate) tpu: Option<String>,
    pub(crate) ip: Option<String>,
    pub(crate) port: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LeaderLocationPayload {
    pub(crate) slot: u64,
    pub(crate) leader: String,
    pub(crate) ip: Option<String>,
    pub(crate) port: Option<String>,
    pub(crate) latitude: Option<f64>,
    pub(crate) longitude: Option<f64>,
    pub(crate) city: Option<String>,
    pub(crate) country: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LeaderPathPayload {
    pub(crate) current_slot: u64,
    pub(crate) limit: usize,
    pub(crate) slot_ms: u64,
    pub(crate) ts: u64,
    pub(crate) path: Vec<LeaderLocationPayload>,
}

#[derive(Clone)]
pub(crate) struct NodeInfo {
    pub(crate) tpu: Option<String>,
}

#[derive(Default)]
pub(crate) struct NodesCache {
    pub(crate) nodes_by_pubkey: HashMap<String, NodeInfo>,
    pub(crate) ts_ms: u64,
}

#[derive(Default)]
pub(crate) struct LeaderCache {
    pub(crate) start_slot: Option<u64>,
    pub(crate) leaders: Vec<String>,
}

impl LeaderCache {
    pub(crate) fn reset(&mut self) {
        self.start_slot = None;
        self.leaders.clear();
    }

    pub(crate) fn end_slot(&self) -> Option<u64> {
        self.start_slot
            .and_then(|start| start.checked_add(self.leaders.len().saturating_sub(1) as u64))
    }

    pub(crate) fn slots_until_leader(&self, track: &str, slot: u64) -> Option<u64> {
        let start = self.start_slot?;
        if slot < start {
            return None;
        }
        let offset = (slot - start) as usize;
        if offset >= self.leaders.len() {
            return None;
        }
        let index = self
            .leaders
            .iter()
            .skip(offset)
            .position(|leader| leader == track)?;
        Some(index as u64)
    }
}

#[derive(Clone)]
pub(crate) struct TrackSchedule {
    pub(crate) epoch_start: u64,
    pub(crate) slots_in_epoch: u64,
    pub(crate) slots: Vec<u64>,
}

impl TrackSchedule {
    pub(crate) fn covers_slot(&self, slot: u64) -> bool {
        slot >= self.epoch_start && slot < self.epoch_start.saturating_add(self.slots_in_epoch)
    }

    pub(crate) fn slots_until(&self, slot: u64) -> Option<u64> {
        if !self.covers_slot(slot) || self.slots.is_empty() {
            return None;
        }
        let index = match self.slots.binary_search(&slot) {
            Ok(index) => index,
            Err(index) => index,
        };
        self.slots
            .get(index)
            .and_then(|next| next.checked_sub(slot))
    }
}

#[derive(Clone, Serialize)]
#[allow(non_snake_case)]
pub(crate) struct BasePayload {
    pub(crate) slot: u64,
    pub(crate) slotTimestamp: u64,
    pub(crate) currentValidator: Option<String>,
    pub(crate) nextValidator: Option<String>,
    pub(crate) currentTpu: Option<String>,
    pub(crate) currentIp: Option<String>,
    pub(crate) currentPort: Option<String>,
    pub(crate) nextTpu: Option<String>,
    pub(crate) nextIp: Option<String>,
    pub(crate) nextPort: Option<String>,
    pub(crate) slotsUntilSwitch: Option<u64>,
}

#[derive(Serialize)]
#[allow(non_snake_case)]
pub(crate) struct PayloadNoTrack {
    slot: u64,
    slotTimestamp: u64,
    currentValidator: Option<String>,
    nextValidator: Option<String>,
    currentTpu: Option<String>,
    currentIp: Option<String>,
    currentPort: Option<String>,
    nextTpu: Option<String>,
    nextIp: Option<String>,
    nextPort: Option<String>,
    slotsUntilSwitch: Option<u64>,
}

impl PayloadNoTrack {
    pub(crate) fn from_base(base: BasePayload) -> Self {
        Self {
            slot: base.slot,
            slotTimestamp: base.slotTimestamp,
            currentValidator: base.currentValidator,
            nextValidator: base.nextValidator,
            currentTpu: base.currentTpu,
            currentIp: base.currentIp,
            currentPort: base.currentPort,
            nextTpu: base.nextTpu,
            nextIp: base.nextIp,
            nextPort: base.nextPort,
            slotsUntilSwitch: base.slotsUntilSwitch,
        }
    }
}

#[derive(Serialize)]
#[allow(non_snake_case)]
pub(crate) struct Payload {
    slot: u64,
    slotTimestamp: u64,
    currentValidator: Option<String>,
    nextValidator: Option<String>,
    currentTpu: Option<String>,
    currentIp: Option<String>,
    currentPort: Option<String>,
    nextTpu: Option<String>,
    nextIp: Option<String>,
    nextPort: Option<String>,
    slotsUntilSwitch: Option<u64>,
    slotsUntilLeader: Option<u64>,
}

impl Payload {
    pub(crate) fn from_base(base: BasePayload, slots_until_leader: Option<u64>) -> Self {
        Self {
            slot: base.slot,
            slotTimestamp: base.slotTimestamp,
            currentValidator: base.currentValidator,
            nextValidator: base.nextValidator,
            currentTpu: base.currentTpu,
            currentIp: base.currentIp,
            currentPort: base.currentPort,
            nextTpu: base.nextTpu,
            nextIp: base.nextIp,
            nextPort: base.nextPort,
            slotsUntilSwitch: base.slotsUntilSwitch,
            slotsUntilLeader: slots_until_leader,
        }
    }
}
