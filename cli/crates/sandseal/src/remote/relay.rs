use std::sync::Arc;

use anyhow::{Result, Context};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, error};
use url::Url;

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

    /// Connect to the relay and bridge encrypted frames bidirectionally.
    ///
    /// `local_rx`: plaintext bytes from local terminal → encrypt → relay
    /// `local_tx`: relay → decrypt → plaintext bytes to local terminal
    pub async fn run(
        self,
        session_keys: Arc<Mutex<SessionKeys>>,
        mut local_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        local_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<()> {
        let mut url = Url::parse(&self.relay_url)
            .context("invalid relay URL")?;
        url.query_pairs_mut()
            .append_pair("token", &self.relay_token)
            .append_pair("role", "cli");

        info!("connecting to relay: {}", url.host_str().unwrap_or("unknown"));

        let (ws_stream, _) = connect_async(url.as_str())
            .await
            .context("failed to connect to relay")?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        info!("connected to relay");

        let send_keys = session_keys.clone();
        let send_handle = tokio::spawn(async move {
            while let Some(data) = local_rx.recv().await {
                let frame = {
                    let mut keys = send_keys.lock().await;
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
                            MessageType::Close => {
                                info!("received close from browser");
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
        info!("relay connection closed");
        Ok(())
    }
}
