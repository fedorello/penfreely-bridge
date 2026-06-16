//! `penfreely-bridge`: runs a user's local Ollama models for the hosted service.
//!
//! Opens an outbound websocket to the backend (so it works behind NAT with no
//! inbound ports), authenticates with a bridge token, advertises the local
//! models, and streams inference back. Reconnects with backoff if the link drops.

mod backoff;
mod config;
mod ollama;
mod protocol;
mod session;

use std::sync::Arc;
use std::time::Duration;

use crate::protocol::{ClientFrame, ServerFrame};
use futures::{SinkExt, StreamExt};
use tokio::signal;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::backoff::Backoff;
use crate::config::{AgentConfig, SystemEnv};
use crate::ollama::{HttpOllama, Ollama};
use crate::session::run_session;

/// Default tracing filter when `RUST_LOG` is unset.
const DEFAULT_LOG_FILTER: &str = "info";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = AgentConfig::from_env(&SystemEnv)?;
    tracing::info!(backend = %config.backend_ws_url, ollama = %config.ollama_url, "bridge agent starting");

    let http = reqwest::Client::builder().build()?;
    let ollama: Arc<dyn Ollama> = Arc::new(HttpOllama::new(config.ollama_url.clone(), http));
    let mut backoff = Backoff::new(config.backoff_initial, config.backoff_max);

    loop {
        tokio::select! {
            () = shutdown_signal() => {
                tracing::info!("shutting down");
                break;
            }
            result = connect_once(&config, ollama.clone()) => {
                match result {
                    Ok(()) => {
                        tracing::info!("disconnected; will reconnect");
                        backoff.reset();
                    }
                    Err(error) => tracing::warn!(%error, "connection failed; will retry"),
                }
                let delay = backoff.next_delay();
                if wait_or_shutdown(delay).await {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Connect once and run the session until the link drops. Returns `Ok` on a
/// clean disconnect, `Err` if connecting or the handshake failed.
async fn connect_once(config: &AgentConfig, ollama: Arc<dyn Ollama>) -> anyhow::Result<()> {
    let mut request = config.backend_ws_url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert(AUTHORIZATION, format!("Bearer {}", config.token).parse()?);
    let (socket, _response) = connect_async(request).await?;
    tracing::info!("connected to backend");
    let (mut sink, mut stream) = socket.split();

    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<ServerFrame>();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<ClientFrame>();

    // Writer: serialize outgoing client frames onto the socket.
    let writer = tokio::spawn(async move {
        while let Some(frame) = outgoing_rx.recv().await {
            let text = serde_json::to_string(&frame)?;
            sink.send(Message::Text(text.into())).await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    // Reader: decode incoming server frames from the socket.
    let reader = tokio::spawn(async move {
        while let Some(message) = stream.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    if let Ok(frame) = serde_json::from_str::<ServerFrame>(&text)
                        && incoming_tx.send(frame).is_err()
                    {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    let result = run_session(ollama, incoming_rx, outgoing_tx).await;
    reader.abort();
    let _ = writer.await;
    result
}

/// Wait for `delay`, returning `true` if a shutdown signal arrived first.
async fn wait_or_shutdown(delay: Duration) -> bool {
    tokio::select! {
        () = shutdown_signal() => true,
        () = tokio::time::sleep(delay) => false,
    }
}

/// Resolve when the process is asked to stop (Ctrl-C).
async fn shutdown_signal() {
    let _ = signal::ctrl_c().await;
}

/// Initialize tracing from `RUST_LOG`, defaulting to `info`.
fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
