use std::env;
use std::time::Duration;

use anyhow::Result;
use tracing::warn;

use crate::constants::{
    DEFAULT_HEARTBEAT_MS, DEFAULT_LEADER_LOOKAHEAD, DEFAULT_NODE_CACHE_TTL_MS, DEFAULT_PORT,
    DEFAULT_REQUEST_TIMEOUT_MS, DEFAULT_RPC_URL, DEFAULT_TRACK_LOOKAHEAD, DEFAULT_WS_PING_MS,
};

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) rpc_url: String,
    pub(crate) rpc_x_token: Option<String>,
    pub(crate) ws_url: String,
    pub(crate) ws_x_token: Option<String>,
    pub(crate) port: u16,
    pub(crate) request_timeout: Duration,
    pub(crate) node_cache_ttl: Duration,
    pub(crate) heartbeat: Duration,
    pub(crate) ws_ping_interval: Duration,
    pub(crate) leader_lookahead: usize,
    pub(crate) track_lookahead: usize,
    pub(crate) maxmind_db_path: String,
    pub(crate) maxmind_license_key: Option<String>,
    pub(crate) maxmind_edition_id: String,
    pub(crate) maxmind_db_download_url: Option<String>,
    pub(crate) maxmind_fallback_url: Option<String>,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        let rpc_override = read_env_first(&["SOLANA_RPC_URL"]);
        let using_default_rpc = rpc_override.is_none();
        let rpc_url = rpc_override
            .clone()
            .unwrap_or_else(|| DEFAULT_RPC_URL.to_string());
        if using_default_rpc {
            warn!("SOLANA_RPC_URL not set; defaulting to {}", DEFAULT_RPC_URL);
        }
        let rpc_x_token = read_env_first(&["SOLANA_RPC_X_TOKEN"]);
        let ws_override = read_env_first(&["SOLANA_WSS_URL", "SOLANA_WS_URL"]);
        let using_default_ws = ws_override.is_none() && using_default_rpc;
        let ws_url = ws_override
            .as_deref()
            .map(derive_ws_url)
            .unwrap_or_else(|| derive_ws_url(&rpc_url));
        if using_default_ws {
            warn!(
                "SOLANA_WSS_URL not set; defaulting to {}",
                derive_ws_url(DEFAULT_RPC_URL)
            );
        }
        let ws_x_token = read_env_first(&["SOLANA_WS_X_TOKEN"]).or_else(|| rpc_x_token.clone());

        let port = env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        let request_timeout = Duration::from_millis(
            env::var("RPC_TIMEOUT_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        );

        let node_cache_ttl = Duration::from_millis(
            env::var("NODE_CACHE_TTL_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(DEFAULT_NODE_CACHE_TTL_MS),
        );

        let heartbeat = Duration::from_millis(
            env::var("SSE_HEARTBEAT_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(DEFAULT_HEARTBEAT_MS),
        );

        let ws_ping_interval = Duration::from_millis(
            read_env_first(&["WS_PING_MS", "GRPC_PING_MS"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(DEFAULT_WS_PING_MS),
        );

        let leader_lookahead = env::var("LEADER_LOOKAHEAD")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_LEADER_LOOKAHEAD);

        let track_lookahead = env::var("TRACK_LOOKAHEAD")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_TRACK_LOOKAHEAD);

        let maxmind_db_path =
            env::var("MAXMIND_DB_PATH").unwrap_or_else(|_| "./GeoLite2-City.mmdb".to_string());
        let maxmind_license_key = read_env_first(&["MAXMIND_LICENSE_KEY", "GEOIP_LICENSE_KEY"]);
        let maxmind_edition_id =
            env::var("MAXMIND_EDITION_ID").unwrap_or_else(|_| "GeoLite2-City".to_string());
        let maxmind_db_download_url = read_env_first(&["MAXMIND_DB_DOWNLOAD_URL"]);
        let maxmind_fallback_url = read_env_first(&["MAXMIND_FALLBACK_URL"]);

        Ok(Self {
            rpc_url,
            rpc_x_token,
            ws_url,
            ws_x_token,
            port,
            request_timeout,
            node_cache_ttl,
            heartbeat,
            ws_ping_interval,
            leader_lookahead,
            track_lookahead,
            maxmind_db_path,
            maxmind_license_key,
            maxmind_edition_id,
            maxmind_db_download_url,
            maxmind_fallback_url,
        })
    }
}

pub(crate) fn read_env_first(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

pub(crate) fn derive_ws_url(rpc_url: &str) -> String {
    let mut url = match url::Url::parse(rpc_url) {
        Ok(url) => url,
        Err(_) => return rpc_url.to_string(),
    };
    match url.scheme() {
        "http" => {
            let _ = url.set_scheme("ws");
        }
        "https" => {
            let _ = url.set_scheme("wss");
        }
        "ws" | "wss" => {}
        _ => return rpc_url.to_string(),
    }
    url.to_string()
}
