use std::path::Path;
use std::time::Duration;

use serde_json::json;

/// Per-session memory credential, obtained on the host at `sandseal start`.
///
/// Everything here is best-effort by design: the free tier has no memory, and neither a
/// logged-out user nor an unreachable backend may stop a sandbox from starting. A failure
/// means "this session has no memory", never an error.
pub struct MemorySession {
    pub id: String,
    pub token: String,
    /// Resolved here so the container is told exactly the backend the host talked to.
    pub api_url: String,
}

const TIMEOUT: Duration = Duration::from_secs(10);

pub async fn open(api_url: Option<&str>, project_dir: &Path, instance_name: &str) -> Option<MemorySession> {
    let token = match crate::auth::token::load_token() {
        Ok(Some(token)) if !token.is_expired() => token,
        _ => {
            tracing::debug!("no valid login — starting without memory");
            return None;
        }
    };

    let project_name = project_dir.file_name()?.to_string_lossy().to_string();
    let base = crate::cli::resolve_api_url(api_url).trim_end_matches('/');

    let client = reqwest::Client::builder().timeout(TIMEOUT).build().ok()?;
    let response = client
        .post(format!("{base}/api/sessions"))
        .bearer_auth(&token.access_token)
        .json(&json!({
            "projectName": project_name,
            "projectDir": project_dir.to_string_lossy(),
            "instanceName": instance_name,
        }))
        .send()
        .await;

    let body: serde_json::Value = match response {
        Ok(resp) if resp.status().is_success() => resp.json().await.ok()?,
        Ok(resp) => {
            tracing::debug!("session request returned {} — starting without memory", resp.status());
            return None;
        }
        Err(err) => {
            tracing::debug!("session request failed ({err}) — starting without memory");
            return None;
        }
    };

    // A backend without memory configured returns a session but no credential. That is a
    // valid answer, not an error.
    let memory_token = body.get("memoryToken").and_then(serde_json::Value::as_str)?;
    let id = body.get("id").and_then(serde_json::Value::as_str)?;

    Some(MemorySession {
        id: id.to_string(),
        token: memory_token.to_string(),
        api_url: base.to_string(),
    })
}

/// Ends the session, which revokes the credential immediately — the whole reason it is
/// database-backed rather than signed. Best-effort: an unreachable backend leaves the session
/// marked running and it expires by other means, but the sandbox must still shut down cleanly.
pub async fn close(api_url: Option<&str>, session_id: &str) {
    let Ok(Some(token)) = crate::auth::token::load_token() else { return };
    let base = crate::cli::resolve_api_url(api_url).trim_end_matches('/');

    let Ok(client) = reqwest::Client::builder().timeout(TIMEOUT).build() else { return };
    let result = client
        .patch(format!("{base}/api/sessions/{session_id}"))
        .bearer_auth(&token.access_token)
        .json(&json!({ "status": "stopped" }))
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => tracing::debug!("memory session {session_id} closed"),
        Ok(resp) => tracing::debug!("closing memory session returned {}", resp.status()),
        Err(err) => tracing::debug!("closing memory session failed: {err}"),
    }
}
