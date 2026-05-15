use anyhow::{Result, Context, bail};
use serde::Deserialize;
use tracing::info;

use crate::auth::token::{AuthToken, save_token};

const DEVICE_AUTH_URL: &str = "https://sandseal.io/api/auth/device";
const TOKEN_URL: &str = "https://sandseal.io/api/auth/device/token";

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

/// RFC 8628 OAuth Device Authorization Flow.
///
/// 1. Request device + user codes
/// 2. Display URL + user code
/// 3. Poll for token until authorized or expired
pub async fn login(api_url: Option<&str>) -> Result<()> {
    let base = api_url.unwrap_or("https://sandseal.io");
    let device_url = format!("{base}/api/auth/device");
    let poll_url = format!("{base}/api/auth/device/token");

    let client = reqwest::Client::new();

    // Step 1: Request device code
    let resp: DeviceCodeResponse = client
        .post(&device_url)
        .json(&serde_json::json!({ "client_id": "sandseal-cli" }))
        .send()
        .await
        .context("failed to request device code")?
        .json()
        .await
        .context("invalid device code response")?;

    println!();
    println!("  Open this URL in your browser:");
    println!("  {}", resp.verification_uri);
    println!();
    println!("  Enter code: {}", resp.user_code);
    println!();
    println!("  Waiting for authorization...");

    // Step 2: Poll for token
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(resp.expires_in);
    let interval = std::time::Duration::from_secs(resp.interval.max(5));

    loop {
        if std::time::Instant::now() > deadline {
            bail!("device authorization expired");
        }

        tokio::time::sleep(interval).await;

        let poll: TokenResponse = client
            .post(&poll_url)
            .json(&serde_json::json!({
                "client_id": "sandseal-cli",
                "device_code": resp.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }))
            .send()
            .await
            .context("failed to poll for token")?
            .json()
            .await
            .context("invalid token response")?;

        if let Some(access_token) = poll.access_token {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let token = AuthToken {
                access_token,
                refresh_token: poll.refresh_token,
                expires_at: poll.expires_in.map(|e| now + e),
            };

            save_token(&token)?;
            info!("authenticated successfully");
            println!("  Logged in successfully!");
            return Ok(());
        }

        match poll.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Some("expired_token") => bail!("device authorization expired"),
            Some("access_denied") => bail!("authorization denied by user"),
            Some(e) => bail!("authorization error: {e}"),
            None => continue,
        }
    }
}
