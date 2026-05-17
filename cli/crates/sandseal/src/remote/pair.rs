use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::info;
use url::Url;

use crate::auth::token::require_valid_token;
use crate::crypto::keys::ensure_identity;
use crate::crypto::pairing::{
    complete_qr_pairing, create_qr_offer, generate_pairing_password, PasswordPairing,
};

const DEFAULT_API_URL: &str = "https://sandseal.io";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairSessionResponse {
    session_id: String,
    relay_url: String,
    relay_token: String,
}

async fn create_pair_session(api_url: &str, mode: &str) -> Result<PairSessionResponse> {
    let token = require_valid_token()?;
    let client = reqwest::Client::new();

    let resp: PairSessionResponse = client
        .post(format!("{api_url}/api/pair"))
        .bearer_auth(&token.access_token)
        .json(&serde_json::json!({ "mode": mode }))
        .send()
        .await
        .context("failed to create pairing session")?
        .error_for_status()
        .context("API returned error")?
        .json()
        .await
        .context("invalid pairing response")?;

    Ok(resp)
}

async fn connect_relay(relay_url: &str, relay_token: &str) -> Result<(
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
)> {
    let mut url = Url::parse(relay_url).context("invalid relay URL")?;
    url.query_pairs_mut().append_pair("role", "cli");

    let (ws_stream, _) = connect_async(url.as_str())
        .await
        .context("failed to connect to relay")?;

    let (mut sink, source) = ws_stream.split();

    sink.send(Message::Text(relay_token.to_string().into()))
        .await
        .context("failed to send auth token")?;

    info!("connected to relay for pairing");
    Ok((sink, source))
}

/// Wait for a single binary message from the relay.
async fn recv_binary(
    source: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
) -> Result<Vec<u8>> {
    loop {
        match source.next().await {
            Some(Ok(Message::Binary(data))) => return Ok(data.to_vec()),
            Some(Ok(Message::Close(_))) => bail!("relay closed connection"),
            Some(Err(e)) => bail!("WebSocket error: {e}"),
            None => bail!("connection closed"),
            _ => continue,
        }
    }
}

fn save_paired_device(device_name: &str, their_identity: &[u8; 32], shared_secret: &[u8; 32]) -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home")?;
    let dir = home.join(".sandseal").join("paired");
    std::fs::create_dir_all(&dir)?;

    let data = serde_json::json!({
        "identity": base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            their_identity,
        ),
        "shared_secret": base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            shared_secret,
        ),
    });

    let path = dir.join(format!("{device_name}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&data)?)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    info!("saved paired device: {}", path.display());
    Ok(())
}

pub async fn pair_qr(api_url: Option<&str>) -> Result<()> {
    let base = api_url.unwrap_or(DEFAULT_API_URL);
    let identity = ensure_identity()?;

    println!("  Creating pairing session...");
    let session = create_pair_session(base, "qr").await?;

    let (offer, ephemeral) = create_qr_offer(&identity);
    let pair_url = format!(
        "{}&session={}",
        offer.to_url(base),
        session.session_id,
    );

    println!();
    println!("  Open in browser:");
    println!("  {pair_url}");
    println!();
    println!("  Verification code: {}", offer.verification_code);
    println!();
    println!("  Waiting for browser to connect...");

    let (mut sink, mut source) = connect_relay(&session.relay_url, &session.relay_token).await?;

    // Send our ephemeral pubkey to relay (browser will receive it)
    sink.send(Message::Binary(ephemeral.public.as_bytes().to_vec().into()))
        .await
        .context("failed to send ephemeral key")?;

    // Wait for browser's ephemeral pubkey (32 bytes)
    let their_eph_bytes = recv_binary(&mut source).await?;
    if their_eph_bytes.len() != 32 {
        bail!("invalid browser ephemeral key length: {}", their_eph_bytes.len());
    }
    let mut their_eph = [0u8; 32];
    their_eph.copy_from_slice(&their_eph_bytes);

    // Wait for browser's identity pubkey (32 bytes)
    let their_id_bytes = recv_binary(&mut source).await?;
    if their_id_bytes.len() != 32 {
        bail!("invalid browser identity key length: {}", their_id_bytes.len());
    }
    let mut their_identity = [0u8; 32];
    their_identity.copy_from_slice(&their_id_bytes);

    // Complete pairing
    let result = complete_qr_pairing(&identity, &ephemeral, &their_eph, &their_identity)?;

    println!("  Browser connected!");
    println!("  Verification: {}", result.verification_code);

    // Send our signature of their identity key (proves we hold our private key)
    sink.send(Message::Binary(result.our_signature.to_vec().into()))
        .await
        .context("failed to send signature")?;

    save_paired_device("browser", &their_identity, &result.shared_secret)?;

    println!();
    println!("  Pairing complete.");
    Ok(())
}

pub async fn pair_password(api_url: Option<&str>) -> Result<()> {
    let base = api_url.unwrap_or(DEFAULT_API_URL);
    let identity = ensure_identity()?;

    println!("  Creating pairing session...");
    let session = create_pair_session(base, "password").await?;

    let password = generate_pairing_password();
    let pairing = PasswordPairing::initiate(&password)?;

    println!();
    println!("  Password: {password}");
    println!("  Session:  {}", session.session_id);
    println!();
    println!("  Enter this password at: {base}/pair/{}", session.session_id);
    println!();
    println!("  Waiting for browser to connect...");

    let (mut sink, mut source) = connect_relay(&session.relay_url, &session.relay_token).await?;

    // Send salt to browser (16 bytes, unencrypted — salt is public)
    sink.send(Message::Binary(pairing.salt.to_vec().into()))
        .await
        .context("failed to send salt")?;

    // Wait for browser to send its encrypted identity key
    let their_encrypted_id = recv_binary(&mut source).await?;

    let their_identity = pairing.decrypt_identity(&their_encrypted_id)
        .context("failed to decrypt browser identity — wrong password?")?;

    println!("  Browser connected! Identity verified.");

    // Send our encrypted identity key
    let our_encrypted_id = pairing.encrypt_identity(&identity)?;
    sink.send(Message::Binary(our_encrypted_id.into()))
        .await
        .context("failed to send encrypted identity")?;

    save_paired_device("browser", &their_identity, pairing.shared_secret())?;

    println!();
    println!("  Pairing complete.");
    Ok(())
}
