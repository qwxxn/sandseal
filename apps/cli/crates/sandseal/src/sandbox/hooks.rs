use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use tracing::{debug, info, warn};

use crate::config::schema::ScriptHook;
use crate::path::resolve::resolve_host_path;

/// Execute setupHost hooks on the host before Docker work.
pub fn run_setup_host_hooks(hooks: &[ScriptHook], project_dir: &Path) -> Result<()> {
    for hook in hooks {
        let script_path = resolve_host_path(&hook.script, project_dir);
        if !script_path.exists() {
            warn!("setupHost script not found: {}", script_path.display());
            continue;
        }

        info!("running setupHost hook: {}", script_path.display());

        let status = Command::new("bash")
            .arg(&script_path)
            .current_dir(project_dir)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("failed to execute setupHost hook: {}", script_path.display()))?;

        if !status.success() {
            bail!(
                "setupHost hook failed (exit {}): {}",
                status.code().unwrap_or(-1),
                script_path.display()
            );
        }
    }

    Ok(())
}

/// Execute cleanupHost hooks on the host after teardown.
/// Non-fatal: logs warnings on failure but doesn't abort.
pub fn run_cleanup_host_hooks(hooks: &[ScriptHook], project_dir: &Path) {
    for hook in hooks {
        let script_path = resolve_host_path(&hook.script, project_dir);
        if !script_path.exists() {
            warn!("cleanupHost script not found: {}", script_path.display());
            continue;
        }

        debug!("running cleanupHost hook: {}", script_path.display());

        match Command::new("bash")
            .arg(&script_path)
            .current_dir(project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
        {
            Ok(status) if !status.success() => {
                warn!(
                    "cleanupHost hook failed (exit {}): {}",
                    status.code().unwrap_or(-1),
                    script_path.display()
                );
            }
            Err(e) => {
                warn!("failed to execute cleanupHost hook: {e}");
            }
            _ => {}
        }
    }
}
