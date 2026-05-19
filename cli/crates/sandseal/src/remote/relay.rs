use anyhow::{Result, Context, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, error};
use url::Url;
use x25519_dalek::PublicKey as X25519Public;

use crate::crypto::keys::X25519KeyPair;
use crate::crypto::session::{SessionKeys, MessageType};
use super::overlay::OverlayMessage;

pub struct RelayClient {
    relay_url: String,
    relay_token: String,
}

impl RelayClient {
    pub fn new(relay_url: String, relay_token: String) -> Self {
        Self {
            relay_url,
            relay_token,
        }
    }

    /// Connect to relay, perform key exchange, then bridge encrypted frames.
    ///
    /// Protocol:
    /// 1. Send token as first text message (auth)
    /// 2. Send ephemeral X25519 pubkey (32 bytes) as first binary message
    /// 3. Receive peer's pubkey (32 bytes) as first binary message from them
    /// 4. Derive SessionKeys, switch to encrypted framing
    ///
    /// If the browser reconnects (page refresh), it sends a new 32-byte pubkey.
    /// The CLI detects this (encrypted frames are always >= 33 bytes) and re-keys.
    pub async fn connect_and_run(
        self,
        mut local_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        local_tx: mpsc::UnboundedSender<Vec<u8>>,
        overlay_tx: Option<mpsc::UnboundedSender<OverlayMessage>>,
    ) -> Result<()> {
        let mut url = Url::parse(&self.relay_url)
            .context("invalid relay URL")?;
        url.query_pairs_mut().append_pair("role", "cli");

        let overlay = |msg: OverlayMessage| {
            if let Some(tx) = &overlay_tx {
                let _ = tx.send(msg);
            }
        };

        info!("connecting to relay: {}", url.host_str().unwrap_or("unknown"));
        overlay(OverlayMessage::info("connecting to relay…"));

        let (ws_stream, _) = connect_async(url.as_str())
            .await
            .context("failed to connect to relay")?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Step 1: Authenticate
        ws_sink.send(Message::Text(self.relay_token.clone().into()))
            .await
            .context("failed to send auth token")?;

        info!("authenticated with relay");
        overlay(OverlayMessage::info("authenticated with relay"));

        // Step 2: Key exchange — send our ephemeral pubkey
        let ephemeral = X25519KeyPair::generate();
        ws_sink.send(Message::Binary(ephemeral.public.as_bytes().to_vec().into()))
            .await
            .context("failed to send ephemeral pubkey")?;

        info!("sent ephemeral pubkey, waiting for peer...");
        overlay(OverlayMessage::info("waiting for browser…"));

        // Step 3: Receive peer's ephemeral pubkey
        let peer_pubkey = loop {
            match ws_source.next().await {
                Some(Ok(Message::Binary(data))) => {
                    if data.len() != 32 {
                        bail!("invalid peer pubkey length: {}", data.len());
                    }
                    let mut key_bytes = [0u8; 32];
                    key_bytes.copy_from_slice(&data);
                    break X25519Public::from(key_bytes);
                }
                Some(Ok(Message::Close(_))) => bail!("relay closed before key exchange"),
                Some(Err(e)) => bail!("WebSocket error during key exchange: {e}"),
                None => bail!("connection closed before key exchange"),
                _ => continue,
            }
        };

        info!("key exchange complete, session encrypted");
        overlay(OverlayMessage::info("session encrypted"));

        // Step 4: Derive session keys (CLI is always the initiator)
        let mut session_keys =
            SessionKeys::from_key_exchange(&ephemeral, &peer_pubkey, true);

        // Single select! loop — no shared state, no race conditions on re-key
        loop {
            tokio::select! {
                Some(data) = local_rx.recv() => {
                    if session_keys.needs_rotation() {
                        if let Ok(rf) = session_keys.seal(MessageType::KeyRotation, &[]) {
                            let _ = ws_sink.send(Message::Binary(rf.into())).await;
                        }
                        session_keys.rotate();
                        info!("key rotation performed (gen {})", session_keys.generation());
                    }
                    match session_keys.seal(MessageType::Data, &data) {
                        Ok(f) => {
                            if ws_sink.send(Message::Binary(f.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("encrypt error: {e}");
                            break;
                        }
                    }
                }
                msg = ws_source.next() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            match session_keys.open(&data) {
                                Ok((MessageType::Data, plaintext)) => {
                                    if local_tx.send(plaintext).is_err() {
                                        break;
                                    }
                                }
                                Ok((MessageType::KeyRotation, _)) => {
                                    session_keys.rotate();
                                    info!("peer rotated keys (gen {})", session_keys.generation());
                                }
                                Ok((MessageType::Close, _)) => {
                                    info!("received close from peer");
                                    break;
                                }
                                Ok(_) => {}
                                Err(_) if data.len() == 32 => {
                                    info!("browser reconnected, performing re-key exchange");
                                    overlay(OverlayMessage::info("browser reconnected"));
                                    let new_ephemeral = X25519KeyPair::generate();
                                    if ws_sink.send(Message::Binary(
                                        new_ephemeral.public.as_bytes().to_vec().into()
                                    )).await.is_err() {
                                        break;
                                    }

                                    let mut key_bytes = [0u8; 32];
                                    key_bytes.copy_from_slice(&data);
                                    let peer_pub = X25519Public::from(key_bytes);
                                    session_keys = SessionKeys::from_key_exchange(
                                        &new_ephemeral, &peer_pub, true,
                                    );
                                    info!("re-key exchange complete");
                                    overlay(OverlayMessage::info("session re-encrypted"));
                                }
                                Err(e) => {
                                    error!("decrypt error: {e}");
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(e)) => {
                            error!("WebSocket error: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        info!("relay connection closed (sent {} messages)", session_keys.send_count());
        overlay(OverlayMessage::info("relay disconnected"));
        Ok(())
    }
}
