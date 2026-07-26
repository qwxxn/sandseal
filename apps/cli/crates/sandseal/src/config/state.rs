use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Mutable local state — which profile is active. Not part of `settings.json`, because
/// it is machine-local and per-checkout (belongs in `.gitignore`, not in the repo).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Project => write!(f, "project"),
            Scope::Global => write!(f, "global"),
        }
    }
}

pub fn global_state_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".sandseal/state.json"))
}

pub fn project_state_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".sandseal/state.json")
}

pub fn state_path(project_dir: &Path, scope: Scope) -> Result<PathBuf> {
    match scope {
        Scope::Project => Ok(project_state_path(project_dir)),
        Scope::Global => global_state_path(),
    }
}

pub fn load(path: &Path) -> Result<State> {
    if !path.is_file() {
        return Ok(State::default());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("invalid JSON in {}", path.display()))
}

pub fn save(path: &Path, state: &State) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let content = serde_json::to_string_pretty(state)?;
    std::fs::write(path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Resolve the active profile for a project: project state wins, global state is the fallback.
pub fn active_profile(project_dir: &Path) -> Result<Option<(String, Scope)>> {
    if let Some(name) = load(&project_state_path(project_dir))?.active_profile {
        return Ok(Some((name, Scope::Project)));
    }
    if let Some(name) = load(&global_state_path()?)?.active_profile {
        return Ok(Some((name, Scope::Global)));
    }
    Ok(None)
}

/// Write the active profile (or clear it when `name` is `None`) at the given scope.
pub fn set_active_profile(project_dir: &Path, scope: Scope, name: Option<&str>) -> Result<PathBuf> {
    let path = state_path(project_dir, scope)?;
    let mut state = load(&path)?;
    state.active_profile = name.map(str::to_string);
    save(&path, &state)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_state() {
        let state = load(Path::new("/nonexistent/sandseal/state.json")).unwrap();
        assert!(state.active_profile.is_none());
    }

    #[test]
    fn roundtrip_uses_camel_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".sandseal/state.json");

        save(
            &path,
            &State {
                active_profile: Some("night".into()),
            },
        )
        .unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("activeProfile"));
        assert_eq!(load(&path).unwrap().active_profile.as_deref(), Some("night"));
    }

    #[test]
    fn clearing_removes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();

        set_active_profile(project, Scope::Project, Some("review")).unwrap();
        assert_eq!(
            load(&project_state_path(project)).unwrap().active_profile.as_deref(),
            Some("review")
        );

        set_active_profile(project, Scope::Project, None).unwrap();
        assert!(load(&project_state_path(project)).unwrap().active_profile.is_none());
    }

    #[test]
    fn project_state_wins_over_global() {
        // Only the project layer is exercised here — the global layer lives in $HOME
        // and is covered by the resolution order in `active_profile`.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        set_active_profile(project, Scope::Project, Some("untrusted")).unwrap();

        let (name, scope) = active_profile(project).unwrap().unwrap();
        assert_eq!(name, "untrusted");
        assert_eq!(scope, Scope::Project);
    }
}
