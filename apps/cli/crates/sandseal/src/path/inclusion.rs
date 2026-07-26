use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

use super::resolve::{resolve_container_path, resolve_host_path, strip_trailing_slash};

/// A resolved file inclusion: host path → container path.
#[derive(Debug, Clone)]
pub struct InclusionMount {
    pub host_path: PathBuf,
    pub container_path: PathBuf,
}

impl InclusionMount {
    pub fn to_volume_mount(&self) -> String {
        format!("{}:{}", self.host_path.display(), self.container_path.display())
    }
}

/// Resolve inclusion entries from settings into volume mounts.
pub fn resolve_inclusions(
    entries: &HashMap<String, String>,
    project_dir: &Path,
    sandbox_home: &str,
) -> Vec<InclusionMount> {
    let mut mounts = Vec::new();

    for (host_raw, container_raw) in entries {
        let host_raw = strip_trailing_slash(host_raw);
        let container_raw = strip_trailing_slash(container_raw);

        let host_path = resolve_host_path(host_raw, project_dir);
        let container_path = resolve_container_path(container_raw, sandbox_home, project_dir);

        if !host_path.exists() {
            warn!("inclusion host path does not exist: {}", host_path.display());
            continue;
        }

        mounts.push(InclusionMount {
            host_path,
            container_path,
        });
    }

    mounts
}
