use std::sync::Arc;

use anyhow::{Result, Context};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use futures_util::{SinkExt, StreamExt};
use tracing::{info, error};

use crate::crypto::session::SessionKeys;
use crate::remote::relay::RelayClient;

/// Spawn ttyd for the sandbox tmux session and bridge it to the relay.
pub async fn bridge_terminal(
    tmux_session: &str,
    ttyd_port: u16,
    relay_url: String,
    relay_token: String,
    session_keys: Arc<Mutex<SessionKeys>>,
) -> Result<()> {
    // Start ttyd pointing at the tmux session
    let mut ttyd = Command::new("ttyd")
        .args([
            "--port",
            &ttyd_port.to_string(),
            "--writable",
            "--once",
            "tmux",
            "attach-session",
            "-t",
            tmux_session,
        ])
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn ttyd")?;

    // Give ttyd a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    info!("ttyd started on port {ttyd_port}");

    // Connect to local ttyd WebSocket
    let ttyd_url = format!("ws://127.0.0.1:{ttyd_port}/ws");
    let (ttyd_ws, _) = connect_async(&ttyd_url)
        .await
        .context("failed to connect to ttyd")?;

    let (mut ttyd_sink, mut ttyd_source) = ttyd_ws.split();

    // Channels between ttyd and relay
    let (relay_to_ttyd_tx, mut relay_to_ttyd_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (ttyd_to_relay_tx, ttyd_to_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // ttyd → relay direction: read from ttyd WS, send to relay channel
    let ttyd_read_handle = tokio::spawn(async move {
        while let Some(Ok(msg)) = ttyd_source.next().await {
            if let tokio_tungstenite::tungstenite::Message::Binary(data) = msg {
                if ttyd_to_relay_tx.send(data.to_vec()).is_err() {
                    break;
                }
            }
        }
    });

    // relay → ttyd direction: read from relay channel, write to ttyd WS
    let ttyd_write_handle = tokio::spawn(async move {
        while let Some(data) = relay_to_ttyd_rx.recv().await {
            if ttyd_sink
                .send(tokio_tungstenite::tungstenite::Message::Binary(data.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Relay connection — bridges encrypted relay ↔ plaintext ttyd channels
    let relay = RelayClient::new(relay_url, relay_token);
    let relay_result = relay.run(session_keys, ttyd_to_relay_rx, relay_to_ttyd_tx).await;

    ttyd_read_handle.abort();
    ttyd_write_handle.abort();
    let _ = ttyd.kill().await;

    relay_result
}
