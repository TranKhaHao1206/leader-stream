mod background;
mod config;
mod constants;
mod handlers;
mod models;
mod rpc;
mod server;
mod state;
mod util;

#[cfg(test)]
mod tests;

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rustls::crypto::ring::default_provider;
use rustls::crypto::CryptoProvider;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::background::{run_leader_cache_updater, run_slot_informer, run_subscriber_metrics};
use crate::config::{read_env_first, Config};
use crate::constants::DEFAULT_STATIC_DIR;
use crate::rpc::RpcClient;
use crate::server::build_router;
use crate::state::AppState;

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "leader_stream=info".into()),
        )
        .init();

    CryptoProvider::install_default(default_provider())
        .map_err(|_| anyhow!("Failed to install rustls crypto provider"))?;

    let config = Config::from_env()?;
    let rpc = RpcClient::new(
        config.rpc_url.clone(),
        config.rpc_x_token.clone(),
        config.request_timeout,
    )?;
    let leader_stream_url =
        read_env_first(&["NEXT_PUBLIC_LEADER_STREAM_URL"]).unwrap_or_else(|| {
            "/api/leader-stream".to_string()
        });
    let state = AppState::new(config.clone(), rpc, leader_stream_url);

    let disable_background = env::var("DISABLE_BACKGROUND_TASKS")
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty() && trimmed != "0"
        })
        .unwrap_or(false);

    if disable_background {
        warn!("background tasks disabled via DISABLE_BACKGROUND_TASKS");
    } else {
        tokio::spawn(run_slot_informer(Arc::clone(&state)));
        tokio::spawn(run_leader_cache_updater(Arc::clone(&state)));
        tokio::spawn(run_subscriber_metrics(Arc::clone(&state)));
    }

    let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| DEFAULT_STATIC_DIR.to_string());

    let app = build_router(Arc::clone(&state), static_dir);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind {}", addr))?;

    info!("leader-stream listening on {}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(Arc::clone(&state)))
    .await
    .context("server error")?;

    Ok(())
}

async fn shutdown_signal(state: Arc<AppState>) {
    #[cfg(unix)]
    {
        let ctrl_c = tokio::signal::ctrl_c();
        let terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => Some(signal),
            Err(err) => {
                warn!(?err, "failed to install SIGTERM handler");
                None
            }
        };
        let quit = match signal(SignalKind::quit()) {
            Ok(signal) => Some(signal),
            Err(err) => {
                warn!(?err, "failed to install SIGQUIT handler");
                None
            }
        };

        tokio::select! {
            _ = ctrl_c => {},
            _ = async {
                if let Some(mut signal) = terminate {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {},
            _ = async {
                if let Some(mut signal) = quit {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }

    state.broadcast_shutdown();
}
