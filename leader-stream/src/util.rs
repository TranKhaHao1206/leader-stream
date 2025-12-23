use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;

pub(crate) struct TpuAddress {
    pub(crate) ip: Option<String>,
    pub(crate) port: Option<String>,
}

pub(crate) fn parse_tpu_address(tpu: Option<&str>) -> TpuAddress {
    let value = match tpu {
        Some(value) => value.trim(),
        None => return TpuAddress { ip: None, port: None },
    };

    if value.is_empty() {
        return TpuAddress { ip: None, port: None };
    }

    if value.contains("://") {
        if let Ok(url) = Url::parse(value) {
            return TpuAddress {
                ip: url.host_str().map(|host| host.to_string()),
                port: url.port().map(|port| port.to_string()),
            };
        }
    }

    if value.starts_with('[') {
        if let Some(end) = value.find(']') {
            let ip = value.get(1..end).map(|slice| slice.to_string());
            let rest = value.get(end + 1..).unwrap_or("");
            if rest.starts_with(':') {
                return TpuAddress {
                    ip,
                    port: rest
                        .get(1..)
                        .filter(|port| !port.is_empty())
                        .map(|port| port.to_string()),
                };
            }
            return TpuAddress { ip, port: None };
        }
    }

    if let Some(last_colon) = value.rfind(':') {
        if last_colon > 0 && last_colon < value.len().saturating_sub(1) {
            return TpuAddress {
                ip: Some(value[..last_colon].to_string()),
                port: Some(value[last_colon + 1..].to_string()),
            };
        }
    }

    TpuAddress {
        ip: Some(value.to_string()),
        port: None,
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
