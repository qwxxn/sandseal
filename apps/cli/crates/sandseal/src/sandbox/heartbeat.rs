//! Proof that the CLI driving a sandbox is still running.
//!
//! The backend hears from a session exactly twice: started, and stopped. Everything that
//! kills the CLI without letting it speak — a closed terminal, `kill -9`, a lost machine —
//! leaves the session marked running for good. A beat on a timer turns that silence into
//! something the server's collector can act on.
//!
//! Best-effort throughout, like every other call the CLI makes to the backend: a sandbox
//! runs with no login and no network at all, and a failed beat only costs accuracy.

use std::time::Duration;

use serde_json::json;
use tokio::task::JoinHandle;
use tracing::debug;

/// One beat a minute against the server's five-minute grace: four may go missing to a flaky
/// network before anything is concluded from the silence.
const INTERVAL: Duration = Duration::from_secs(60);

const TIMEOUT: Duration = Duration::from_secs(10);

/// Beats until the returned handle is dropped or aborted.
pub fn spawn(api_url: Option<String>, session_id: String) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(client) = reqwest::Client::builder().timeout(TIMEOUT).build() else { return };

        loop {
            tokio::time::sleep(INTERVAL).await;
            beat(&client, api_url.as_deref(), &session_id).await;
        }
    })
}

async fn beat(client: &reqwest::Client, api_url: Option<&str>, session_id: &str) {
    // Read per beat rather than once: a session can outlive the token that opened it, and
    // re-reading is how a beat starts working again after `sandseal login`.
    let Ok(Some(token)) = crate::auth::token::load_token() else { return };
    if token.is_expired() {
        return;
    }

    let base = crate::cli::resolve_api_url(api_url).trim_end_matches('/');
    let result = client
        .patch(format!("{base}/api/sessions/{session_id}"))
        // No fields: the request itself is the message. The server stamps the time it
        // arrived, so a heartbeat cannot claim a moment that never happened.
        .bearer_auth(&token.access_token)
        .json(&json!({}))
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => debug!("heartbeat for {session_id}"),
        Ok(resp) => debug!("heartbeat returned {}", resp.status()),
        Err(err) => debug!("heartbeat failed: {err}"),
    }
}
