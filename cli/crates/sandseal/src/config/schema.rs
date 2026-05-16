use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,

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
