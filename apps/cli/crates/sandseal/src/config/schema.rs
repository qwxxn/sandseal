use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,

    /// Human-readable label, shown by `sandseal config list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<FileSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HookSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<HashMap<String, String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<DockerSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gc: Option<GcSettings>,
}

impl Settings {
    /// Whether `sandseal start` sweeps abandoned sandboxes before starting one.
    ///
    /// Opt-out: an abandoned sandbox is a container still running an agent, so leaving one
    /// behind is the outcome that needs asking for, not the other way round.
    pub fn gc_on_start(&self) -> bool {
        self.gc
            .as_ref()
            .and_then(|gc| gc.on_start)
            .unwrap_or(true)
    }
}

/// When the collector runs on its own.
///
/// Only `onStart` exists so far — `sandseal gc` is explicit and always runs — but it is nested
/// from the start for the same reason `memory.scope` is: moving a flattened key later breaks
/// every settings file that already uses it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GcSettings {
    /// Sweep before starting a sandbox. Defaults to true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_start: Option<bool>,
}

/// Memory configuration for this sandbox.
///
/// Only `scope` exists so far. The rest of the planned surface — `enabled`, `space`,
/// `retrieval`, `redaction` — lands here as it is built, which is why scope is nested from the
/// start rather than flattened and moved later.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<MemoryScopeSettings>,
}

/// Which slice of memory this sandbox writes to and reads from.
///
/// Both fields exist because the project a note belongs to is otherwise the name of the
/// directory the sandbox was started in, and that is wrong in two directions: a sandbox opened
/// over several projects gets the parent directory's name, and one project gets a different
/// slice depending on where you launched from — the same knowledge then splits into buckets
/// that cannot see each other.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryScopeSettings {
    /// Name notes are filed under. Defaults to the project directory's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    /// Whether recall reads the whole space or stays inside this project. Defaults to the whole
    /// space. Writes are pinned to the project either way, so attribution survives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_project: Option<bool>,
}

impl MemorySettings {
    pub fn project(&self) -> Option<&str> {
        self.scope.as_ref()?.project.as_deref()
    }

    pub fn cross_project(&self) -> Option<bool> {
        self.scope.as_ref()?.cross_project
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_swap_limit: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_image: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupHook>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prestart: Option<Vec<ScriptHook>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_host: Option<Vec<ScriptHook>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_host: Option<Vec<ScriptHook>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetupHook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptHook {
    pub script: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    pub dir: String,

    #[serde(default)]
    pub readwrite: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DockerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> Settings {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn the_start_sweep_is_on_unless_someone_turns_it_off() {
        // Opt-out: settings that say nothing, and settings with an unrelated `gc` shape,
        // both sweep. Only an explicit false does not.
        assert!(parse(serde_json::json!({})).gc_on_start());
        assert!(parse(serde_json::json!({"gc": {}})).gc_on_start());
        assert!(parse(serde_json::json!({"gc": {"onStart": true}})).gc_on_start());
        assert!(!parse(serde_json::json!({"gc": {"onStart": false}})).gc_on_start());
    }
}
