use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

use super::resolve::{expand_env_vars, expand_glob, has_glob_chars, resolve_host_path, strip_trailing_slash};

/// A resolved file exclusion, ready to become a Docker volume mount.
#[derive(Debug, Clone)]
pub enum ExclusionMount {
    /// File hidden via /dev/null mount
    File(PathBuf),
    /// Directory hidden via empty dir bind mount
    Directory(PathBuf),
}

impl ExclusionMount {
    /// Generate docker-compose volume mount string.
    pub fn to_volume_mount(&self, tmp_dir: &Path) -> String {
        match self {
            ExclusionMount::File(path) => {
                format!("/dev/null:{}:ro", path.display())
            }
            ExclusionMount::Directory(path) => {
                let mount_subdir = tmp_dir.join("excluded-dirs").join(
                    path.to_string_lossy().trim_start_matches('/')
                );
                std::fs::create_dir_all(&mount_subdir).ok();
                format!("{}:{}", mount_subdir.display(), path.display())
            }
        }
    }
}

/// Resolve exclusion entries from settings into volume mounts.
/// Handles glob expansion, env vars, deduplication.
pub fn resolve_exclusions(
    entries: &[String],
    project_dir: &Path,
) -> Result<Vec<ExclusionMount>> {
    let mut seen = HashSet::new();
    let mut mounts = Vec::new();

    for entry in entries {
        let entry = strip_trailing_slash(entry);
        let entry = expand_env_vars(entry);

        let resolved_paths = if has_glob_chars(&entry) {
            expand_glob(&entry, project_dir)?
        } else {
            let path = resolve_host_path(&entry, project_dir);
            if !path.exists() {
                warn!("exclusion path does not exist: {}", path.display());
                continue;
            }
            vec![path]
        };

        if resolved_paths.is_empty() {
            warn!("exclusion pattern matched no files: {entry}");
            continue;
        }

        for path in resolved_paths {
            let canonical = path.to_string_lossy().to_string();
            if seen.contains(&canonical) {
                continue;
            }
            seen.insert(canonical);

            if path.is_file() {
                mounts.push(ExclusionMount::File(path));
            } else if path.is_dir() {
                mounts.push(ExclusionMount::Directory(path));
            } else {
                warn!("exclusion path is neither file nor directory: {}", path.display());
            }
        }
    }

    Ok(mounts)
}
