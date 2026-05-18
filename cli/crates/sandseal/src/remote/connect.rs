use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

use crate::auth::token::require_valid_token;
use crate::cli;
use crate::crypto::keys::ensure_identity;
use crate::remote::relay::RelayClient;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    id: String,
    relay_token: String,
    relay_url: String,
}

pub async fn connect(project_dir: &str, api_url: Option<&str>) -> Result<()> {
    let token = require_valid_token()?;
    let identity = ensure_identity()?;

    let base = cli::resolve_api_url(api_url);
    let url = format!("{base}/api/sessions");

    info!("creating session for {project_dir}");
    println!("  Creating session...");

    let client = reqwest::Client::new();
    let resp: CreateSessionResponse = client
        .post(&url)
        .bearer_auth(&token.access_token)
        .json(&serde_json::json!({
            "projectName": project_dir.split('/').last().unwrap_or("unknown"),
            "projectDir": project_dir,
            "instanceName": format!("connect-{}", &identity.public_key_base64()[..8]),
        }))
        .send()
        .await
        .context("failed to create session")?
        .error_for_status()
        .context("API returned error")?
        .json()
        .await
        .context("invalid session response")?;

    info!("session created: {}", resp.id);
    println!("  Session: {}", resp.id);
    println!("  Relay: {}", resp.relay_url);
    println!("  Waiting for browser to connect...");

    // Connect to relay and perform key exchange with browser.
    // The relay-brokered protocol:
    //   1. Auth with relay token (first text message)
    //   2. Send ephemeral X25519 pubkey (32 bytes binary)
    //   3. Receive browser's ephemeral pubkey
    //   4. Derive shared SessionKeys → encrypted communication
    let relay = RelayClient::new(resp.relay_url, resp.relay_token);

    // For `sandseal connect`, stdin/stdout bridge to relay
    let (local_tx, mut local_rx_out) = mpsc::unbounded_channel::<Vec<u8>>();
    let (local_tx_in, local_rx_in) = mpsc::unbounded_channel::<Vec<u8>>();

    // Pipe relay output → stdout
    let output_handle = tokio::spawn(async move {
        while let Some(data) = local_rx_out.recv().await {
            if let Ok(text) = String::from_utf8(data) {
                print!("{text}");
            }
        }
    });

    // Pipe stdin → relay input
    let input_handle = tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let stdin = tokio::io::stdin();
        let mut reader = tokio::io::BufReader::new(stdin).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if local_tx_in.send(line.into_bytes()).is_err() {
                break;
            }
        }
    });

    relay.connect_and_run(local_rx_in, local_tx).await?;

    output_handle.abort();
    input_handle.abort();

    println!("\n  Session ended.");
    Ok(())
}
