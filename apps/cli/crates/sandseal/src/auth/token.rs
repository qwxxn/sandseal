use std::path::PathBuf;

use anyhow::{bail, Result, Context};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

impl AuthToken {
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                now >= exp
            }
            None => false,
        }
    }
}

fn token_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("sandseal")
        .join("auth.json")
}

pub fn load_token() -> Result<Option<AuthToken>> {
    let path = token_path();
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .context("failed to read auth token")?;
    let token: AuthToken = serde_json::from_str(&data)
        .context("failed to parse auth token")?;
    Ok(Some(token))
}

pub fn save_token(token: &AuthToken) -> Result<()> {
    let path = token_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(token)?;
    std::fs::write(&path, data)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

pub fn clear_token() -> Result<()> {
    let path = token_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn require_valid_token() -> Result<AuthToken> {
    let token = load_token()?
        .context("not logged in — run `sandseal login` first")?;
    if token.is_expired() {
        bail!("session expired — run `sandseal login` to re-authenticate");
    }
    Ok(token)
}
