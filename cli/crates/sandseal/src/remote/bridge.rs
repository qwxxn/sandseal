use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, debug};

use crate::remote::relay::RelayClient;

/// Attach to a running Docker container and bridge its stdin/stdout to the relay.
pub async fn bridge_container(
    container_name: &str,
    relay_url: String,
    relay_token: String,
) -> Result<()> {
    let mut child = Command::new("docker")
        .args(["attach", container_name])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to attach to container")?;

    let stdout = child.stdout.take().context("no stdout")?;
    let stdin = child.stdin.take().context("no stdin")?;

    let (to_relay_tx, to_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (from_relay_tx, mut from_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Container stdout → relay
    let tx = to_relay_tx.clone();
    let read_handle = tokio::spawn(async move {
        let mut reader = stdout;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        debug!("container stdout ended");
    });

    // Relay → container stdin
    let write_handle = tokio::spawn(async move {
        let mut writer = stdin;
        while let Some(data) = from_relay_rx.recv().await {
            if writer.write_all(&data).await.is_err() {
                break;
            }
        }
        debug!("relay input ended");
    });

    info!("bridging container to relay");
    let relay = RelayClient::new(relay_url, relay_token);
    let result = relay.connect_and_run(to_relay_rx, from_relay_tx).await;

    read_handle.abort();
    write_handle.abort();
    let _ = child.kill().await;

    result
}
