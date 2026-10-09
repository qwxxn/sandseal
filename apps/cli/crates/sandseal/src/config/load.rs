use anyhow::{Context, Result};
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
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

/// A settings file that took part in the merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerSource {
    Global(PathBuf),
    /// A `.sandseal/settings.json` in a directory above the project.
    Inherited(PathBuf),
    Project(PathBuf),
}

impl LayerSource {
    pub fn path(&self) -> &Path {
        match self {
            LayerSource::Global(p) | LayerSource::Inherited(p) | LayerSource::Project(p) => p,
        }
    }
}

impl fmt::Display for LayerSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            LayerSource::Global(_) => "global",
            LayerSource::Inherited(_) => "inherited",
            LayerSource::Project(_) => "project",
        };
        write!(f, "{} ({kind})", self.path().display())
    }
}

pub struct Resolved {
    pub settings: Settings,
    /// The same layers minus the project's own `settings.json` and the ones inherited from
    /// directories above it — the slice of the configuration that is identical for every
    /// project on this machine. What can be built once and shared (see `docker::image`) is
    /// decided from this, not from `settings`, which no longer says where a value came from.
    pub shared: Settings,
    pub value: Value,
    pub profile: Option<(String, ProfileSource)>,
    /// Settings files merged, lowest precedence first. The profile is in `profile`.
    pub sources: Vec<LayerSource>,
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

/// Key that stops the lookup through parent directories at the file that sets it, like
/// `root = true` in `.editorconfig`. Stripped from the result — it steers the lookup, it is
/// not a setting.
const ROOT_KEY: &str = "root";

/// Load the effective settings.
///
/// Layers, lowest precedence first: global `settings.json` carries machine-wide defaults,
/// every `.sandseal/settings.json` in the directories above the project adds what a group of
/// repositories shares (farthest first), the project adds its own specifics, and the profile
/// lands on top — it is the lock, so a project cannot re-open what a profile closed. A layer
/// removes inherited values with `$replace` (see `merge::merge_layer`).
pub fn resolve(project_dir: &Path, choice: &ProfileChoice) -> Result<Resolved> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    resolve_in(project_dir, &home, choice)
}

fn resolve_in(project_dir: &Path, home: &Path, choice: &ProfileChoice) -> Result<Resolved> {
    let selected = resolve_profile(project_dir, choice)?;

    let mut sources = Vec::new();
    let mut layers = Vec::new();
    // Only the machine-wide layers: global and the profile.
    let mut shared_layers = Vec::new();

    let global = home.join(".sandseal/settings.json");
    if global.exists() {
        let layer = validate_settings(&global)?;
        shared_layers.push(layer.clone());
        layers.push(layer);
        sources.push(LayerSource::Global(global));
    }

    for (source, layer) in directory_layers(project_dir, home)? {
        layers.push(layer);
        sources.push(source);
    }

    for source in &sources {
        debug!("settings layer: {source}");
    }

    if let Some((name, source)) = &selected {
        // A missing profile is a hard error — silently skipping it would drop
        // whatever restrictions the profile was there to enforce.
        let layer = profile::load(name)?;
        shared_layers.push(layer.clone());
        layers.push(layer);
        debug!("settings layer: profile '{name}' (from {source})");
    }

    let mut value = merge_layers(&layers);
    if let Some(obj) = value.as_object_mut() {
        obj.remove(ROOT_KEY);
    }
    let settings =
        serde_json::from_value(value.clone()).context("failed to deserialize merged settings")?;
    let shared = serde_json::from_value(merge_layers(&shared_layers))
        .context("failed to deserialize machine-wide settings")?;

    Ok(Resolved {
        settings,
        shared,
        value,
        profile: selected,
        sources,
    })
}

/// The project's `.sandseal/settings.json` and every one above it, lowest precedence first.
///
/// The walk stops below `$HOME` — `~/.sandseal/settings.json` is the global layer and must
/// not be merged twice — and at the first file that sets `"root": true`. A project outside
/// `$HOME` is walked up to `/`.
fn directory_layers(project_dir: &Path, home: &Path) -> Result<Vec<(LayerSource, Value)>> {
    let mut found = Vec::new();

    for dir in project_dir.ancestors().take_while(|dir| *dir != home) {
        let path = dir.join(".sandseal/settings.json");
        if !path.is_file() {
            continue;
        }

        let layer = validate_settings(&path)?;
        let root = layer.get(ROOT_KEY) == Some(&Value::Bool(true));

        let source = if dir == project_dir {
            LayerSource::Project(path)
        } else {
            LayerSource::Inherited(path)
        };
        found.push((source, layer));

        if root {
            break;
        }
    }

    found.reverse();
    Ok(found)
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

    fn write_settings(dir: &Path, value: Value) {
        std::fs::create_dir_all(dir.join(".sandseal")).unwrap();
        std::fs::write(
            dir.join(".sandseal/settings.json"),
            serde_json::to_string(&value).unwrap(),
        )
        .unwrap();
    }

    fn source_dirs(layers: &[(LayerSource, Value)]) -> Vec<&Path> {
        layers
            .iter()
            .map(|(source, _)| source.path().parent().unwrap().parent().unwrap())
            .collect()
    }

    /// The group config set once for `~/development/apps` reaches every repo under it,
    /// farthest first so the nearer directory overrides it.
    #[test]
    fn ancestors_layer_farthest_first_below_the_project() {
        let home = tempfile::tempdir().unwrap();
        let dev = home.path().join("development");
        let apps = dev.join("apps");
        let project = apps.join("web");
        write_settings(&dev, json!({"network": {"mode": "bridge"}}));
        write_settings(&apps, json!({"network": {"mode": "host"}, "dependencies": ["jq"]}));
        write_settings(&project, json!({"dependencies": ["ripgrep"]}));

        let layers = directory_layers(&project, home.path()).unwrap();

        assert_eq!(source_dirs(&layers), [dev.as_path(), apps.as_path(), project.as_path()]);
        assert!(matches!(layers[0].0, LayerSource::Inherited(_)));
        assert!(matches!(layers[2].0, LayerSource::Project(_)));

        let merged = merge_layers(&layers.into_iter().map(|(_, v)| v).collect::<Vec<_>>());
        assert_eq!(merged["network"]["mode"], json!("host"));
        assert_eq!(merged["dependencies"], json!(["jq", "ripgrep"]));
    }

    /// `~/.sandseal/settings.json` is the global layer — reading it again as an ancestor
    /// would concatenate every array in it twice.
    #[test]
    fn walk_stops_below_home() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("project");
        write_settings(home.path(), json!({"dependencies": ["jq"]}));
        std::fs::create_dir_all(&project).unwrap();

        assert!(directory_layers(&project, home.path()).unwrap().is_empty());
        // Opening a sandbox over $HOME itself must not load the global file as a project.
        assert!(directory_layers(home.path(), home.path()).unwrap().is_empty());
    }

    #[test]
    fn root_stops_the_walk() {
        let home = tempfile::tempdir().unwrap();
        let dev = home.path().join("development");
        let apps = dev.join("apps");
        let project = apps.join("web");
        write_settings(&dev, json!({"environment": {"FROM_DEV": "1"}}));
        write_settings(&apps, json!({"root": true, "environment": {"FROM_APPS": "1"}}));
        std::fs::create_dir_all(&project).unwrap();

        let layers = directory_layers(&project, home.path()).unwrap();
        assert_eq!(source_dirs(&layers), [apps.as_path()]);

        // `root` in the project itself cuts every ancestor off.
        write_settings(&project, json!({"root": true}));
        let layers = directory_layers(&project, home.path()).unwrap();
        assert_eq!(source_dirs(&layers), [project.as_path()]);
    }

    #[test]
    fn root_false_keeps_walking() {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("apps");
        let project = apps.join("web");
        write_settings(&apps, json!({}));
        write_settings(&project, json!({"root": false}));

        let layers = directory_layers(&project, home.path()).unwrap();
        assert_eq!(source_dirs(&layers), [apps.as_path(), project.as_path()]);
    }

    /// A repo has to be able to drop what its parent directory handed down, the same way a
    /// profile drops what global handed down.
    #[test]
    fn project_replace_drops_what_an_ancestor_set() {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("apps");
        let project = apps.join("web");
        write_settings(&apps, json!({"environment": {"SHARED_TOKEN": "x"}}));
        write_settings(&project, json!({"$replace": ["environment"], "environment": {"OWN": "y"}}));

        let layers = directory_layers(&project, home.path()).unwrap();
        let merged = merge_layers(&layers.into_iter().map(|(_, v)| v).collect::<Vec<_>>());

        assert_eq!(merged["environment"], json!({"OWN": "y"}));
    }

    #[test]
    fn invalid_ancestor_fails_loudly() {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("apps");
        let project = apps.join("web");
        write_settings(&apps, json!({"netwrok": {}}));
        std::fs::create_dir_all(&project).unwrap();

        let err = directory_layers(&project, home.path()).unwrap_err().to_string();
        assert!(err.contains("apps/.sandseal/settings.json"), "{err}");
    }

    /// `shared` decides the machine-wide base image; a directory's settings only apply to the
    /// repos under it, so baking them into the base would leak them into every other project.
    #[test]
    fn ancestors_stay_out_of_shared() {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("apps");
        let project = apps.join("web");
        write_settings(home.path(), json!({"dependencies": ["jq"]}));
        write_settings(&apps, json!({"root": true, "dependencies": ["postgresql-client"]}));
        std::fs::create_dir_all(&project).unwrap();

        let resolved = resolve_in(&project, home.path(), &ProfileChoice::Disabled).unwrap();

        assert_eq!(
            resolved.settings.dependencies,
            Some(vec!["jq".to_string(), "postgresql-client".to_string()])
        );
        assert_eq!(resolved.shared.dependencies, Some(vec!["jq".to_string()]));
        assert!(resolved.value.get(ROOT_KEY).is_none());
        assert_eq!(
            resolved.sources,
            [
                LayerSource::Global(home.path().join(".sandseal/settings.json")),
                LayerSource::Inherited(apps.join(".sandseal/settings.json")),
            ]
        );
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
