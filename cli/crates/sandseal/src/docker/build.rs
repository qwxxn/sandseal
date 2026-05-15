use anyhow::{Context, Result};
use std::path::Path;
use tracing::debug;

/// Assemble build context: copy entrypoint, setup scripts, agent installs to tmp dir.
pub fn assemble_build_context(
    script_dir: &Path,
    tmp_dir: &Path,
    setup_script: Option<&Path>,
    prestart_scripts: &[(usize, &Path)],
) -> Result<()> {
    // Copy entrypoint
    let src = script_dir.join("agents/entrypoint.sh");
    let dst = tmp_dir.join("entrypoint.sh");
    std::fs::copy(&src, &dst)
        .with_context(|| format!("failed to copy entrypoint: {}", src.display()))?;
    debug!("copied entrypoint to {}", dst.display());

    // Copy agent install scripts
    let agent_installs_src = script_dir.join("agents/claude");
    let agent_installs_dst = tmp_dir.join("agent-installs/claude");
    if agent_installs_src.is_dir() {
        copy_dir_recursive(&agent_installs_src, &agent_installs_dst)?;
        debug!("copied agent installs to {}", agent_installs_dst.display());
    }

    // Setup scripts dir (must always exist for COPY in Dockerfile)
    let setup_dir = tmp_dir.join("setup-scripts");
    std::fs::create_dir_all(&setup_dir)?;
    std::fs::write(setup_dir.join(".gitkeep"), "")?;

    if let Some(setup_path) = setup_script {
        let dst = setup_dir.join("setup.sh");
        std::fs::copy(setup_path, &dst)
            .with_context(|| format!("failed to copy setup script: {}", setup_path.display()))?;
        debug!("copied setup script to {}", dst.display());
    }

    // Copy prestart hook scripts with numbered prefixes
    if !prestart_scripts.is_empty() {
        let prestart_dir = tmp_dir.join("prestart-scripts");
        std::fs::create_dir_all(&prestart_dir)?;
        for (idx, script_path) in prestart_scripts {
            let filename = script_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let dst = prestart_dir.join(format!("{:03}-{filename}", idx));
            std::fs::copy(script_path, &dst)
                .with_context(|| format!("failed to copy prestart script: {}", script_path.display()))?;
            debug!("copied prestart script to {}", dst.display());
        }
    }

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
