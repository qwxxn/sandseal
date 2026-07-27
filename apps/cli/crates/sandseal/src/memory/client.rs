use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// HTTP client for Sandseal's memory endpoints.
///
/// The agent never reaches the memory server: everything goes through Sandseal, which decides
/// the space and project scope. So this client sends no scope of its own — there is nothing
/// here for a prompt injection to redirect.
pub struct MemoryClient {
    base: String,
    credential: String,
    http: reqwest::Client,
}

/// Session credential injected into the sandbox. Never the account token.
const CREDENTIAL_ENV: &str = "SANDSEAL_MEMORY_TOKEN";

/// Whether a search may leave this session's project. Injected alongside the credential, so
/// like the credential it is set by the host and not by anything the agent can reach.
const CROSS_PROJECT_ENV: &str = "SANDSEAL_MEMORY_CROSS_PROJECT";

/// Reads span the whole space unless the host says otherwise. The alternative loses notes
/// outright: a project opened from a different directory lands in a different slice, and its
/// notes then exist but can never be recalled. Writes stay pinned to the session's project
/// regardless of this flag, so a note is still attributed to where the work happened.
fn cross_project() -> bool {
    cross_project_from(std::env::var(CROSS_PROJECT_ENV).ok().as_deref())
}

fn cross_project_from(value: Option<&str>) -> bool {
    match value {
        Some(value) => !matches!(value.trim(), "0" | "false" | "no"),
        None => true,
    }
}

impl MemoryClient {
    /// Prefers the per-session credential from the sandbox environment, falling back to the
    /// stored account token so `sandseal memory` also works on the host.
    pub fn new(api_url: Option<&str>, timeout: Duration) -> Result<Self> {
        let credential = match std::env::var(CREDENTIAL_ENV) {
            Ok(token) if !token.trim().is_empty() => token,
            _ => crate::auth::token::require_valid_token()?.access_token,
        };

        Ok(Self {
            base: crate::cli::resolve_api_url(api_url).trim_end_matches('/').to_string(),
            credential,
            http: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .context("failed to build HTTP client")?,
        })
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self
            .http
            .request(method, format!("{}/api/memory{path}", self.base))
            .bearer_auth(&self.credential);

        if let Some(body) = body {
            req = req.json(&body);
        }

        let resp = req.send().await.context("memory request failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            // Surface the server's own message: it names the missing scope or the reason the
            // subscription was refused, which is what the caller needs to act on.
            anyhow::bail!("memory API returned {status}: {}", text.trim());
        }

        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text).context("memory API returned invalid JSON")
    }

    pub async fn search(
        &self,
        query: &str,
        limit: u32,
        include_linked: bool,
        tags: Option<Vec<String>>,
    ) -> Result<Value> {
        let mut body = json!({
            "query": query,
            "limit": limit,
            "includeLinked": include_linked,
            "crossProject": cross_project(),
        });
        if let Some(tags) = tags {
            body["tags"] = json!(tags);
        }
        self.send(reqwest::Method::POST, "/search", Some(body)).await
    }

    pub async fn add_note(&self, content: &str, tags: Option<Vec<String>>) -> Result<Value> {
        self.send(
            reqwest::Method::POST,
            "/notes",
            Some(json!({ "content": content, "tags": tags.unwrap_or_default() })),
        )
        .await
    }

    pub async fn get_note(&self, id: &str) -> Result<Value> {
        self.send(reqwest::Method::GET, &format!("/notes/{id}"), None).await
    }

    pub async fn update_note(
        &self,
        id: &str,
        content: Option<&str>,
        tags: Option<Vec<String>>,
    ) -> Result<Value> {
        let mut body = json!({});
        if let Some(content) = content {
            body["content"] = json!(content);
        }
        if let Some(tags) = tags {
            body["tags"] = json!(tags);
        }
        self.send(reqwest::Method::PUT, &format!("/notes/{id}"), Some(body)).await
    }

    pub async fn delete_note(&self, id: &str) -> Result<Value> {
        self.send(reqwest::Method::DELETE, &format!("/notes/{id}"), None).await
    }

    pub async fn link_notes(&self, id: &str, target_id: &str, relation: Option<&str>) -> Result<Value> {
        let mut body = json!({ "targetId": target_id });
        if let Some(relation) = relation {
            body["relation"] = json!(relation);
        }
        self.send(reqwest::Method::POST, &format!("/notes/{id}/links"), Some(body))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_without_the_flag_reads_the_whole_space() {
        // The absent case is the one that matters: an older container image carries no such
        // variable, and narrowing its recall would hide notes it used to find.
        assert!(cross_project_from(None));
    }

    #[test]
    fn the_host_can_narrow_recall_to_the_project() {
        assert!(!cross_project_from(Some("0")));
        assert!(!cross_project_from(Some("false")));
        assert!(!cross_project_from(Some(" no ")));
    }

    #[test]
    fn anything_else_keeps_recall_wide() {
        assert!(cross_project_from(Some("1")));
        assert!(cross_project_from(Some("true")));
    }
}
