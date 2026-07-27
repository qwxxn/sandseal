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

pub async fn open(
    api_url: Option<&str>,
    project_dir: &Path,
    instance_name: &str,
    project_override: Option<&str>,
) -> Option<MemorySession> {
    match resolve(api_url, project_dir, instance_name, project_override).await {
        Ok(session) => Some(session),
        Err(reason) => {
            // Info, not debug. Losing memory never stops the sandbox, but a silent
            // loss reads as a broken feature: the only way to learn why was `-d`,
            // which also swaps the agent for a bash shell.
            tracing::info!("memory unavailable: {reason}");
            None
        }
    }
}

async fn resolve(
    api_url: Option<&str>,
    project_dir: &Path,
    instance_name: &str,
    project_override: Option<&str>,
) -> Result<MemorySession, String> {
    let token = match crate::auth::token::load_token() {
        Ok(Some(token)) if !token.is_expired() => token,
        Ok(Some(_)) => return Err("the login expired — run `sandseal login`".into()),
        Ok(None) => return Err("not logged in — run `sandseal login`".into()),
        Err(err) => return Err(format!("cannot read the stored login ({err})")),
    };

    let base = crate::cli::resolve_api_url(api_url).trim_end_matches('/');
    request(&token.access_token, base, project_dir, instance_name, project_override).await
}

/// Every error here becomes the user-facing half of the line above, which is the
/// whole point of it — "memory unavailable" alone is what this was before.
async fn request(
    access_token: &str,
    base: &str,
    project_dir: &Path,
    instance_name: &str,
    project_override: Option<&str>,
) -> Result<MemorySession, String> {
    let project_name = project_name(project_dir, project_override)?;

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|err| format!("cannot build an HTTP client ({err})"))?;

    let response = client
        .post(format!("{base}/api/sessions"))
        .bearer_auth(access_token)
        .json(&json!({
            "projectName": project_name,
            "projectDir": project_dir.to_string_lossy(),
            "instanceName": instance_name,
        }))
        .send()
        .await;

    let body: serde_json::Value = match response {
        Ok(resp) if resp.status().is_success() => resp
            .json()
            .await
            .map_err(|err| format!("{base} returned invalid JSON ({err})"))?,
        // Named separately because it is the one failure a stored token can cause
        // while still looking valid locally: a token signed by another backend
        // verifies nowhere else, which is exactly what a dev login looks like
        // from production.
        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
            return Err(format!("{base} rejected the login — run `sandseal login`"))
        }
        Ok(resp) => return Err(format!("{base} returned {}", resp.status())),
        Err(err) => return Err(format!("{base} is unreachable ({err})")),
    };

    // A backend without memory configured returns a session but no credential. That is a
    // valid answer, not an error.
    let memory_token = body
        .get("memoryToken")
        .and_then(serde_json::Value::as_str)
        .ok_or("this account has no memory")?;
    let id = body
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or("the session response carried no id")?;

    Ok(MemorySession {
        id: id.to_string(),
        token: memory_token.to_string(),
        api_url: base.to_string(),
    })
}

/// Which slice of memory this session writes to.
///
/// The directory name is a guess, and a bad one whenever a sandbox spans several projects: it
/// files everything under the parent directory, and the same project opened from elsewhere
/// lands somewhere else again. `memory.project` is how you say what this sandbox actually is,
/// so it wins outright.
fn project_name(project_dir: &Path, project_override: Option<&str>) -> Result<String, String> {
    if let Some(name) = project_override.map(str::trim).filter(|name| !name.is_empty()) {
        return Ok(name.to_string());
    }

    Ok(project_dir
        .file_name()
        .ok_or("the project directory has no name")?
        .to_string_lossy()
        .to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Answers exactly one request with a canned response, then goes away.
    async fn stub(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}")
    }

    async fn call(base: &str) -> Result<MemorySession, String> {
        request("token", base, Path::new("/tmp/demo-project"), "agent-1", None).await
    }

    #[test]
    fn the_directory_name_is_only_the_fallback() {
        assert_eq!(project_name(Path::new("/home/me/development"), None).unwrap(), "development");
        assert_eq!(
            project_name(Path::new("/home/me/development"), Some("popitchiweb")).unwrap(),
            "popitchiweb"
        );
    }

    #[test]
    fn a_blank_override_is_not_an_override() {
        // `"memory": { "project": "" }` is someone clearing the key, not asking for a
        // nameless project — the server would reject the empty string anyway.
        assert_eq!(project_name(Path::new("/home/me/demo"), Some("  ")).unwrap(), "demo");
        assert_eq!(project_name(Path::new("/home/me/demo"), Some(" api ")).unwrap(), "api");
    }

    /// Not `unwrap_err`: that needs `MemorySession: Debug`, and a struct holding a
    /// live credential has no business being printable.
    fn reason(result: Result<MemorySession, String>) -> String {
        match result {
            Ok(_) => panic!("expected the session to fail"),
            Err(reason) => reason,
        }
    }

    #[tokio::test]
    async fn a_rejected_login_says_which_backend_and_what_to_do() {
        let base = stub("401 Unauthorized", r#"{"error":"unauthorized"}"#).await;
        let err = reason(call(&base).await);
        // The failure a stored token causes while still looking valid locally.
        assert!(err.contains("rejected the login"), "{err}");
        assert!(err.contains("sandseal login"), "{err}");
        assert!(err.contains(&base), "{err}");
    }

    #[tokio::test]
    async fn a_session_without_a_credential_is_reported_as_no_memory() {
        let base = stub("200 OK", r#"{"id":"sess_1"}"#).await;
        let err = reason(call(&base).await);
        assert!(err.contains("no memory"), "{err}");
    }

    #[tokio::test]
    async fn other_failures_carry_the_status() {
        let base = stub("503 Service Unavailable", r#"{}"#).await;
        let err = reason(call(&base).await);
        assert!(err.contains("503"), "{err}");
    }

    #[tokio::test]
    async fn an_unreachable_backend_says_so_instead_of_a_bare_status() {
        // Nothing is listening on this port.
        let err = reason(call("http://127.0.0.1:1").await);
        assert!(err.contains("unreachable"), "{err}");
    }

    #[tokio::test]
    async fn a_complete_response_yields_the_session() {
        let base = stub("200 OK", r#"{"id":"sess_1","memoryToken":"mem_abc"}"#).await;
        let session = call(&base).await.unwrap();
        assert_eq!(session.id, "sess_1");
        assert_eq!(session.token, "mem_abc");
        assert_eq!(session.api_url, base);
    }
}
