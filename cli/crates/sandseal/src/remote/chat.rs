use std::sync::Arc;

use anyhow::{Result, Context};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, error, debug};

use crate::crypto::session::{SessionKeys, MessageType};
use crate::remote::relay::RelayClient;

/// Bridge Claude Code's `--output-format stream-json` output to the relay.
///
/// Spawns Claude Code with the given prompt, reads JSON events from stdout,
/// and forwards each line as an encrypted Data frame to the relay.
/// User input from the browser is forwarded to stdin.
pub async fn bridge_chat(
    project_dir: &str,
    prompt: &str,
    relay_url: String,
    relay_token: String,
    session_keys: Arc<Mutex<SessionKeys>>,
) -> Result<()> {
    let mut child = Command::new("claude")
        .args([
            "--output-format",
            "stream-json",
            "--dangerously-skip-permissions",
            "-p",
            prompt,
        ])
        .current_dir(project_dir)
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn claude")?;

    let stdout = child.stdout.take().context("no stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    let (to_relay_tx, to_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (from_relay_tx, mut from_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Read claude stdout → send to relay as encrypted Data frames
    let tx = to_relay_tx.clone();
    let read_handle = tokio::spawn(async move {
        while let Ok(Some(line)) = reader.next_line().await {
            if line.is_empty() {
                continue;
            }
            debug!("claude event: {}", &line[..line.len().min(80)]);
            let data = line.into_bytes();
            if tx.send(data).is_err() {
                break;
            }
        }
        info!("claude process output ended");
    });

    // Forward relay input → claude stdin (user messages from browser)
    let stdin = child.stdin.take();
    let write_handle = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            use tokio::io::AsyncWriteExt;
            while let Some(data) = from_relay_rx.recv().await {
                if stdin.write_all(&data).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        }
    });

    // Relay connection
    let relay = RelayClient::new(relay_url, relay_token);
    let relay_result = relay.run(session_keys, to_relay_rx, from_relay_tx).await;

    read_handle.abort();
    write_handle.abort();
    let _ = child.kill().await;

    relay_result
}
