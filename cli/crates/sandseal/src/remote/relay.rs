use std::sync::Arc;

use anyhow::{Result, Context, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, error};
use url::Url;
use x25519_dalek::PublicKey as X25519Public;

use crate::crypto::keys::X25519KeyPair;
use crate::crypto::session::{SessionKeys, MessageType};

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
    pub async fn connect_and_run(
        self,
        mut local_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        local_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<()> {
        let mut url = Url::parse(&self.relay_url)
            .context("invalid relay URL")?;
        url.query_pairs_mut().append_pair("role", "cli");

        info!("connecting to relay: {}", url.host_str().unwrap_or("unknown"));

        let (ws_stream, _) = connect_async(url.as_str())
            .await
            .context("failed to connect to relay")?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Step 1: Authenticate
        ws_sink.send(Message::Text(self.relay_token.clone().into()))
            .await
            .context("failed to send auth token")?;

        info!("authenticated with relay");

        // Step 2: Key exchange — send our ephemeral pubkey
        let ephemeral = X25519KeyPair::generate();
        ws_sink.send(Message::Binary(ephemeral.public.as_bytes().to_vec().into()))
            .await
            .context("failed to send ephemeral pubkey")?;

        info!("sent ephemeral pubkey, waiting for peer...");

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

        // Step 4: Derive session keys (CLI is always the initiator)
        let session_keys = Arc::new(Mutex::new(
            SessionKeys::from_key_exchange(&ephemeral, &peer_pubkey, true)
        ));

        // Encrypted communication loop
        let send_keys = session_keys.clone();
        let send_handle = tokio::spawn(async move {
            while let Some(data) = local_rx.recv().await {
                let frame = {
                    let mut keys = send_keys.lock().await;
                    if keys.needs_rotation() {
                        let rotation_frame = keys.seal(MessageType::KeyRotation, &[]);
                        if let Ok(rf) = rotation_frame {
                            let _ = ws_sink.send(Message::Binary(rf.into())).await;
                        }
                        keys.rotate();
                        info!("key rotation performed (gen {})", keys.generation());
                    }
                    keys.seal(MessageType::Data, &data)
                };
                match frame {
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
        });

        while let Some(msg) = ws_source.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    let result = {
                        let mut keys = session_keys.lock().await;
                        keys.open(&data)
                    };
                    match result {
                        Ok((msg_type, plaintext)) => match msg_type {
                            MessageType::Data => {
                                if local_tx.send(plaintext).is_err() {
                                    break;
                                }
                            }
                            MessageType::KeyRotation => {
                                let mut keys = session_keys.lock().await;
                                keys.rotate();
                                info!("peer rotated keys (gen {})", keys.generation());
                            }
                            MessageType::Close => {
                                info!("received close from peer");
                                break;
                            }
                            _ => {}
                        },
                        Err(e) => {
                            error!("decrypt error: {e}");
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    error!("WebSocket error: {e}");
                    break;
                }
                _ => {}
            }
        }

        send_handle.abort();
        let keys = session_keys.lock().await;
        info!("relay connection closed (sent {} messages)", keys.send_count());
        Ok(())
    }
}
