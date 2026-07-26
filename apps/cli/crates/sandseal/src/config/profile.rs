use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

use crate::config::validate::validate_settings;

const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/sandseal/sandseal/main/schema/settings.schema.json";

/// Directory holding named profile definitions.
pub fn profiles_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".sandseal/profiles"))
}

/// Profile names become filenames — keep them boring.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("profile name cannot be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("invalid profile name '{name}': use letters, digits, '-' and '_' only");
    }
    Ok(())
}

pub fn profile_path(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(profiles_dir()?.join(format!("{name}.json")))
}

pub fn exists(name: &str) -> bool {
    profile_path(name).map(|p| p.is_file()).unwrap_or(false)
}

pub struct ProfileInfo {
    pub name: String,
    pub description: Option<String>,
}

/// List profiles sorted by name. Unparseable files are still listed (without a description)
/// so a broken profile stays visible instead of silently vanishing.
pub fn list() -> Result<Vec<ProfileInfo>> {
    let dir = profiles_dir()?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut profiles: Vec<ProfileInfo> = std::fs::read_dir(&dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "json" {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let description = std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<Value>(&c).ok())
                .and_then(|v| v.get("description")?.as_str().map(str::to_string));
            Some(ProfileInfo { name, description })
        })
        .collect();

    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(profiles)
}

/// Load and schema-validate a profile.
pub fn load(name: &str) -> Result<Value> {
    let path = profile_path(name)?;
    if !path.is_file() {
        bail!(
            "profile '{name}' not found. Run `sandseal config list` to see available profiles."
        );
    }
    validate_settings(&path)
}

/// Create a new profile, optionally copying an existing one.
pub fn create(name: &str, from: Option<&str>) -> Result<PathBuf> {
    let path = profile_path(name)?;
    if path.exists() {
        bail!("profile '{name}' already exists: {}", path.display());
    }

    let content = match from {
        Some(source) => {
            let mut value = load(source)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "description".into(),
                    Value::String(format!("copied from {source}")),
                );
            }
            serde_json::to_string_pretty(&value)?
        }
        None => serde_json::to_string_pretty(&serde_json::json!({
            "$schema": SCHEMA_URL,
            "description": format!("{name} profile"),
        }))?,
    };

    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    Ok(path)
}

pub fn delete(name: &str) -> Result<PathBuf> {
    let path = profile_path(name)?;
    if !path.is_file() {
        bail!("profile '{name}' not found");
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("failed to delete {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_names() {
        assert!(validate_name("night").is_ok());
        assert!(validate_name("no-prod_secrets2").is_ok());
    }

    #[test]
    fn rejects_path_traversal_and_junk() {
        assert!(validate_name("").is_err());
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("dot.name").is_err());
        assert!(validate_name("with space").is_err());
    }
}
