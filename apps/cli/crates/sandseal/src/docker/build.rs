use anyhow::{Context, Result};
use std::path::Path;
use tracing::debug;

/// Assemble the base image build context: entrypoint, apt-wrapper, agent installs.
pub fn assemble_base_context(script_dir: &Path, ctx_dir: &Path, agent: &str) -> Result<()> {
    let agents = script_dir.join("agents");

    copy_file(&agents.join("entrypoint.sh"), &ctx_dir.join("entrypoint.sh"))?;
    copy_file(&agents.join("apt-wrapper.sh"), &ctx_dir.join("apt-wrapper.sh"))?;

    let agent_src = agents.join(agent);
    let agent_dst = ctx_dir.join("agent-installs").join(agent);
    if agent_src.is_dir() {
        copy_dir_recursive(&agent_src, &agent_dst)?;
        debug!("copied agent installs to {}", agent_dst.display());
    }

    Ok(())
}

/// Assemble the overlay image build context: the optional setup hook script.
/// The `setup-scripts/` dir must always exist for the COPY in Dockerfile.overlay.
pub fn assemble_overlay_context(ctx_dir: &Path, setup_script: Option<&Path>) -> Result<()> {
    let setup_dir = ctx_dir.join("setup-scripts");
    std::fs::create_dir_all(&setup_dir)?;
    std::fs::write(setup_dir.join(".gitkeep"), "")?;

    if let Some(setup_path) = setup_script {
        copy_file(setup_path, &setup_dir.join("setup.sh"))?;
    }

    Ok(())
}

/// Copy prestart hook scripts (numbered) into the instance tmp dir.
/// These are mounted into the container at runtime, not baked into the image.
pub fn copy_prestart_scripts(tmp_dir: &Path, prestart_scripts: &[(usize, &Path)]) -> Result<()> {
    if prestart_scripts.is_empty() {
        return Ok(());
    }

    let prestart_dir = tmp_dir.join("prestart-scripts");
    std::fs::create_dir_all(&prestart_dir)?;
    for (idx, script_path) in prestart_scripts {
        let filename = script_path.file_name().unwrap_or_default().to_string_lossy();
        let dst = prestart_dir.join(format!("{:03}-{filename}", idx));
        copy_file(script_path, &dst)?;
    }

    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)
        .with_context(|| format!("failed to copy {}", src.display()))?;
    debug!("copied {} -> {}", src.display(), dst.display());
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
