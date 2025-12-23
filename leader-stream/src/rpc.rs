use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Clone)]
pub(crate) struct RpcClient {
    client: reqwest::Client,
    url: String,
    token: Option<String>,
}

impl RpcClient {
    pub(crate) fn new(url: String, token: Option<String>, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("Failed to build RPC client")?;
        Ok(Self { client, url, token })
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }));

        if let Some(token) = &self.token {
            request = request.header("x-token", token);
        }

        let response = request.send().await.context("RPC request failed")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "RPC {} failed ({})",
                method,
                response.status()
            ));
        }

        let payload: RpcResponse<T> = response.json().await.context("Invalid RPC response")?;
        if let Some(error) = payload.error {
            return Err(anyhow!(error.message));
        }

        payload
            .result
            .ok_or_else(|| anyhow!("RPC {} missing result", method))
    }

    pub(crate) async fn get_slot_leaders(&self, start_slot: u64, limit: usize) -> Result<Vec<String>> {
        self.request("getSlotLeaders", serde_json::json!([start_slot, limit]))
            .await
    }

    pub(crate) async fn get_slot(&self) -> Result<u64> {
        self.request(
            "getSlot",
            serde_json::json!([{ "commitment": "processed" }]),
        )
        .await
    }

    pub(crate) async fn get_epoch_info(&self) -> Result<EpochInfo> {
        self.request("getEpochInfo", serde_json::json!([{ "commitment": "processed" }]))
            .await
    }

    pub(crate) async fn get_leader_schedule(&self, slot: u64, identity: &str) -> Result<Option<Vec<u64>>> {
        let result: Option<HashMap<String, Vec<u64>>> = self
            .request(
                "getLeaderSchedule",
                serde_json::json!([slot, { "commitment": "processed", "identity": identity }]),
            )
            .await?;

        Ok(result.and_then(|mut schedule| schedule.remove(identity)))
    }

    pub(crate) async fn get_cluster_nodes(&self) -> Result<Vec<ClusterNode>> {
        self.request("getClusterNodes", serde_json::json!([]))
            .await
    }
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
pub(crate) struct RpcError {
    pub(crate) message: String,
}

#[derive(Deserialize)]
pub(crate) struct JsonRpcEnvelope {
    pub(crate) error: Option<RpcError>,
    pub(crate) id: Option<u64>,
    pub(crate) method: Option<String>,
    pub(crate) params: Option<SlotsUpdateParams>,
    pub(crate) result: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(crate) struct SlotsUpdateParams {
    pub(crate) result: SlotsUpdate,
    pub(crate) subscription: u64,
}

#[derive(Deserialize)]
pub(crate) struct SlotsUpdate {
    pub(crate) slot: u64,
    pub(crate) timestamp: Option<u64>,
    #[serde(rename = "type")]
    pub(crate) update_type: String,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct EpochInfo {
    pub(crate) absoluteSlot: u64,
    pub(crate) slotIndex: u64,
    pub(crate) slotsInEpoch: u64,
}

#[derive(Clone, Deserialize)]
pub(crate) struct ClusterNode {
    pub(crate) pubkey: String,
    pub(crate) tpu: Option<String>,
}
