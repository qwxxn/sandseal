use anyhow::{Result, Context};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, debug, warn};

use crate::remote::relay::RelayClient;

async fn run_claude_turn(
    project_dir: &str,
    prompt: &str,
    is_continuation: bool,
    to_relay_tx: &mpsc::UnboundedSender<Vec<u8>>,
) -> Result<()> {
    let mut args = Vec::new();
    if is_continuation {
        args.push("-c");
    }
    args.extend_from_slice(&[
        "--output-format", "stream-json",
        "--dangerously-skip-permissions",
        "-p", prompt,
    ]);

    let mut child = Command::new("claude")
        .args(&args)
        .current_dir(project_dir)
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn claude")?;

    let stdout = child.stdout.take().context("no stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        if line.is_empty() {
            continue;
        }
        debug!("claude event: {}", &line[..line.len().min(80)]);
        if to_relay_tx.send(line.into_bytes()).is_err() {
            break;
        }
    }

    let status = child.wait().await?;
    if !status.success() {
        anyhow::bail!("claude exited with status: {status}");
    }
    Ok(())
}

/// Bridge Claude Code chat to the relay with multi-turn support.
///
/// Each user message spawns a new `claude` process. The first turn uses `-p`,
/// follow-ups use `-c -p` to continue the conversation. The relay connection
/// stays alive across turns.
pub async fn bridge_chat(
    project_dir: &str,
    prompt: &str,
    relay_url: String,
    relay_token: String,
) -> Result<()> {
    let (to_relay_tx, to_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (from_relay_tx, mut from_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let relay = RelayClient::new(relay_url, relay_token);
    let relay_handle = tokio::spawn(async move {
        relay.connect_and_run(to_relay_rx, from_relay_tx, None).await
    });

    // Echo initial prompt so the browser shows it as a user message
    let _ = to_relay_tx.send(prompt.as_bytes().to_vec());

    // First turn
    match run_claude_turn(project_dir, prompt, false, &to_relay_tx).await {
        Ok(()) => {
            let _ = to_relay_tx.send(b"{\"type\":\"turn_complete\"}".to_vec());
        }
        Err(e) => {
            warn!("claude turn failed: {e}");
            let msg = format!("{{\"type\":\"error\",\"error_message\":{}}}", serde_json::json!(e.to_string()));
            let _ = to_relay_tx.send(msg.into_bytes());
        }
    }

    info!("first turn complete, waiting for follow-up messages");

    // Multi-turn loop: wait for browser messages
    loop {
        match from_relay_rx.recv().await {
            Some(data) => {
                let message = String::from_utf8_lossy(&data).to_string();
                let trimmed = message.trim();
                if trimmed.is_empty() {
                    continue;
                }

                info!("received follow-up ({} bytes)", data.len());

                // Echo user message back so the browser can deduplicate
                let _ = to_relay_tx.send(data);

                match run_claude_turn(project_dir, trimmed, true, &to_relay_tx).await {
                    Ok(()) => {
                        let _ = to_relay_tx.send(b"{\"type\":\"turn_complete\"}".to_vec());
                    }
                    Err(e) => {
                        warn!("claude turn failed: {e}");
                        let msg = format!("{{\"type\":\"error\",\"error_message\":{}}}", serde_json::json!(e.to_string()));
                        let _ = to_relay_tx.send(msg.into_bytes());
                    }
                }
            }
            None => {
                info!("relay disconnected, ending chat session");
                break;
            }
        }
    }

    relay_handle.abort();
    Ok(())
}
