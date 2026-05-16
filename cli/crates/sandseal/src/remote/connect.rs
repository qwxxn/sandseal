use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

use crate::auth::token::require_valid_token;
use crate::crypto::keys::ensure_identity;

const DEFAULT_API_URL: &str = "https://sandseal.io";

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

    let base = api_url.unwrap_or(DEFAULT_API_URL);
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

    // Key exchange between CLI and browser is not yet implemented.
    // The crypto primitives exist (X25519KeyPair, SessionKeys::from_key_exchange),
    // but there is no transport mechanism for the browser to send its ephemeral
    // public key to the CLI. This needs a design decision:
    //   - API-brokered: CLI posts ephemeral key to API, browser retrieves + responds
    //   - Relay-brokered: initial unencrypted handshake over WebSocket
    //
    // Once key exchange is implemented, the flow continues:
    //   1. X25519KeyPair::generate() for ephemeral keypair
    //   2. Exchange ephemeral public keys with browser
    //   3. SessionKeys::from_key_exchange() to derive encryption keys
    //   4. bridge_chat() or RelayClient::run() with session keys

    println!();
    println!("  Key exchange not yet implemented.");
    println!("  Session created but cannot establish encrypted connection.");
    println!("  See: cli/crates/sandseal/src/crypto/pairing.rs for available primitives.");

    Ok(())
}
