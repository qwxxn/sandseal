use anyhow::{Context, Result};
use serde_json::Value;
use std::fmt;
use std::path::Path;
use tracing::debug;

use crate::config::merge::merge_layer;
use crate::config::state::{self, Scope};
use crate::config::validate::validate_settings;
use crate::config::{profile, Settings};

/// Which profile to apply, as requested on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileChoice {
    /// Whatever the state files say (default).
    Active,
    /// Explicit `--profile <name>`.
    Named(String),
    /// `--no-profile` — ignore the active profile for this run.
    Disabled,
}

impl ProfileChoice {
    pub fn from_flags(named: Option<&str>, disabled: bool) -> Self {
        match (named, disabled) {
            (_, true) => ProfileChoice::Disabled,
            (Some(name), false) => ProfileChoice::Named(name.to_string()),
            (None, false) => ProfileChoice::Active,
        }
    }
}

/// Where the applied profile came from — shown to the user so an inherited
/// profile never applies invisibly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Flag,
    State(Scope),
}

impl fmt::Display for ProfileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileSource::Flag => write!(f, "--profile"),
            ProfileSource::State(scope) => write!(f, "{scope}"),
        }
    }
}

pub struct Resolved {
    pub settings: Settings,
    /// The same layers minus the project's own `settings.json` — the slice of the
    /// configuration that is identical for every project on this machine. What can be
    /// built once and shared (see `docker::image`) is decided from this, not from
    /// `settings`, which no longer says where a value came from.
    pub shared: Settings,
    pub value: Value,
    pub profile: Option<(String, ProfileSource)>,
}

/// Resolve which profile applies, without loading it.
pub fn resolve_profile(
    project_dir: &Path,
    choice: &ProfileChoice,
) -> Result<Option<(String, ProfileSource)>> {
    match choice {
        ProfileChoice::Disabled => Ok(None),
        ProfileChoice::Named(name) => Ok(Some((name.clone(), ProfileSource::Flag))),
        ProfileChoice::Active => Ok(state::active_profile(project_dir)?
            .map(|(name, scope)| (name, ProfileSource::State(scope)))),
    }
}

/// Merge the settings layers. Lowest precedence first.
pub fn merge_layers(layers: &[Value]) -> Value {
    layers
        .iter()
        .fold(Value::Object(Default::default()), |acc, layer| {
            merge_layer(&acc, layer)
        })
}

/// Load the effective settings.
///
/// Layers, lowest precedence first: global `settings.json` carries machine-wide defaults,
/// the project adds its own specifics, and the profile lands on top — it is the lock, so
/// a project cannot re-open what a profile closed. A profile removes inherited values with
/// `$replace` (see `merge::merge_layer`).
pub fn resolve(project_dir: &Path, choice: &ProfileChoice) -> Result<Resolved> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let selected = resolve_profile(project_dir, choice)?;

    let mut layers = Vec::new();
    // Everything except the project layer, kept in step with it.
    let mut shared_layers = Vec::new();

    let global = home.join(".sandseal/settings.json");
    if global.exists() {
        let layer = validate_settings(&global)?;
        shared_layers.push(layer.clone());
        layers.push(layer);
        debug!("settings layer: {}", global.display());
    }

    let project = project_dir.join(".sandseal/settings.json");
    if project.exists() {
        layers.push(validate_settings(&project)?);
        debug!("settings layer: {}", project.display());
    }

    if let Some((name, source)) = &selected {
        // A missing profile is a hard error — silently skipping it would drop
        // whatever restrictions the profile was there to enforce.
        let layer = profile::load(name)?;
        shared_layers.push(layer.clone());
        layers.push(layer);
        debug!("settings layer: profile '{name}' (from {source})");
    }

    let value = merge_layers(&layers);
    let settings =
        serde_json::from_value(value.clone()).context("failed to deserialize merged settings")?;
    let shared = serde_json::from_value(merge_layers(&shared_layers))
        .context("failed to deserialize machine-wide settings")?;

    Ok(Resolved {
        settings,
        shared,
        value,
        profile: selected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The layer order is the whole point of the design: global → project → profile.
    #[test]
    fn profile_wins_over_project() {
        let global = json!({"network": {"mode": "host"}, "docker": {"passthrough": true}});
        let project = json!({"network": {"mode": "host"}, "container": {"memoryLimit": "8g"}});
        let profile = json!({"network": {"mode": "bridge"}, "docker": {"passthrough": false}});

        let merged = merge_layers(&[global, project, profile]);

        // The project cannot re-open what the profile closed...
        assert_eq!(merged["network"]["mode"], json!("bridge"));
        assert_eq!(merged["docker"]["passthrough"], json!(false));
        // ...and project specifics the profile says nothing about survive.
        assert_eq!(merged["container"]["memoryLimit"], json!("8g"));
    }

    #[test]
    fn array_layers_concatenate() {
        let global = json!({"files": {"exclude": [".env"]}});
        let project = json!({"files": {"exclude": ["dist"]}});
        let profile = json!({"files": {"exclude": [".env.production", "secrets/"]}});

        let merged = merge_layers(&[global, project, profile]);

        assert_eq!(
            merged["files"]["exclude"],
            json!([".env", "dist", ".env.production", "secrets/"])
        );
    }

    /// The locked-down-run case: a profile has to be able to take inherited secrets away,
    /// not just add to them.
    #[test]
    fn profile_replace_drops_inherited_secrets() {
        let global = json!({
            "environment": {"API_TOKEN": "from-global", "DEPLOY_KEY": "from-global"},
            "files": {
                "exclude": [".env"],
                "include": {"/home/user/.config/creds": "/home/agent/.config/creds"}
            },
            "docker": {"passthrough": true}
        });
        let project = json!({
            "environment": {"PROJECT_VAR": "from-project"},
            "files": {"exclude": ["dist"]}
        });
        let profile = json!({
            "$replace": ["environment", "files.include"],
            "environment": {},
            "files": {"exclude": ["secrets/"]},
            "network": {"mode": "bridge"},
            "docker": {"passthrough": false}
        });

        let merged = merge_layers(&[global, project, profile]);

        // Everything inherited under the replaced paths is gone, from both lower layers.
        assert_eq!(merged["environment"], json!({}));
        assert!(merged["files"].get("include").is_none());
        // Exclusions still accumulate — a profile should only ever hide more, never less.
        assert_eq!(
            merged["files"]["exclude"],
            json!([".env", "dist", "secrets/"])
        );
        assert_eq!(merged["docker"]["passthrough"], json!(false));
        assert_eq!(merged["network"]["mode"], json!("bridge"));
        // The directive never reaches the deserialized settings.
        assert!(merged.get("$replace").is_none());
    }

    #[test]
    fn no_layers_yields_empty_object() {
        assert_eq!(merge_layers(&[]), json!({}));
    }

    #[test]
    fn flags_map_to_choices() {
        assert_eq!(ProfileChoice::from_flags(None, false), ProfileChoice::Active);
        assert_eq!(
            ProfileChoice::from_flags(Some("night"), false),
            ProfileChoice::Named("night".into())
        );
        // --no-profile beats an explicit --profile
        assert_eq!(
            ProfileChoice::from_flags(Some("night"), true),
            ProfileChoice::Disabled
        );
    }
}
