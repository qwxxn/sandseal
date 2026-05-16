use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use crate::cli::{DestroyArgs, StartArgs};
use crate::config::merge::deep_merge;
use crate::config::validate::validate_settings;
use crate::docker::{build, compose, runtime};
use crate::sandbox::cleanup::{register_signal_handler, CleanupGuard};
use crate::sandbox::hooks;

/// Resolve the project directory to an absolute canonical path.
fn resolve_project_dir(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| format!("project directory does not exist: {}", path.display()))
}

/// Create a docker-safe project name: `{basename}-{first8chars(sha1)}`.
fn create_project_name(project_dir: &Path) -> String {
    let dir_str = project_dir.to_string_lossy();
    let mut hasher = Sha1::new();
    hasher.update(dir_str.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let basename = project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-");
    format!("{}-{}", basename, &hash[..8])
}

fn generate_instance_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 3] = rng.random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Find the sandseal script/assets directory.
fn find_script_dir() -> Result<PathBuf> {
    // In development: relative to the binary or SANDSEAL_DIR env var
    if let Ok(dir) = std::env::var("SANDSEAL_DIR") {
        return Ok(PathBuf::from(dir));
    }

    // Try relative to the binary location
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().unwrap();

    // Check for agents/ directory in various locations
    for candidate in &[
        exe_dir.join("../../.."),            // cargo run: target/debug -> project root
        exe_dir.to_path_buf(),               // installed: same dir
        exe_dir.join(".."),                   // installed: parent
        PathBuf::from("."),                   // current dir
    ] {
        let agents = candidate.join("agents");
        if agents.is_dir() {
            return std::fs::canonicalize(candidate)
                .context("failed to canonicalize script dir");
        }
    }

    bail!("cannot find sandseal agents directory. Set SANDSEAL_DIR env var.")
}

pub fn start(args: StartArgs) -> Result<()> {
    let project_dir = resolve_project_dir(&args.path)?;
    let project_name = create_project_name(&project_dir);
    let instance_id = generate_instance_id();
    let instance_name = format!("sandseal-sandbox-{project_name}-{instance_id}");
    let script_dir = find_script_dir()?;

    info!("starting sandbox for {}", project_dir.display());
    debug!("project_name={project_name} instance_name={instance_name}");

    // Create tmp dir
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let tmp_base = home.join(".sandseal/tmp");
    std::fs::create_dir_all(&tmp_base)?;
    let tmp_dir = tempfile::tempdir_in(&tmp_base)?;
    let tmp_path = tmp_dir.keep();

    // Load and merge settings
    let global_settings_path = home.join(".sandseal/settings.json");
    let project_settings_path = project_dir.join(".sandseal/settings.json");
    let mut merged = serde_json::json!({});

    for path in [&global_settings_path, &project_settings_path] {
        if path.exists() {
            let validated = validate_settings(path)?;
            merged = deep_merge(&merged, &validated);
            debug!("merged settings from {}", path.display());
        }
    }

    let settings: crate::config::Settings = serde_json::from_value(merged.clone())
        .context("failed to deserialize merged settings")?;

    // Run setupHost hooks
    if let Some(hooks_cfg) = &settings.hooks {
        if let Some(setup_host) = &hooks_cfg.setup_host {
            hooks::run_setup_host_hooks(setup_host, &project_dir)?;
        }
    }

    // Resolve hooks for build context
    let setup_script = settings.hooks.as_ref()
        .and_then(|h| h.setup.as_ref())
        .and_then(|s| s.script.as_ref())
        .map(|s| crate::path::resolve::resolve_host_path(s, &project_dir));

    let prestart_scripts: Vec<(usize, PathBuf)> = settings.hooks.as_ref()
        .and_then(|h| h.prestart.as_ref())
        .map(|scripts| {
            scripts.iter().enumerate().filter_map(|(i, s)| {
                let path = crate::path::resolve::resolve_host_path(&s.script, &project_dir);
                if path.exists() {
                    Some((i + 1, path))
                } else {
                    tracing::warn!("prestart script not found: {}", path.display());
                    None
                }
            }).collect()
        })
        .unwrap_or_default();

    // Assemble build context
    build::assemble_build_context(
        &script_dir,
        &tmp_path,
        setup_script.as_deref(),
        &prestart_scripts.iter().map(|(i, p)| (*i, p.as_path())).collect::<Vec<_>>(),
    )?;

    // System info
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    let username = std::env::var("USER").unwrap_or_else(|_| "agent".to_string());
    let sandbox_home = home.to_string_lossy().to_string();

    // Generate compose override
    let compose_ctx = compose::ComposeContext {
        project_dir: &project_dir,
        project_name: &project_name,
        instance_name: &instance_name,
        sandbox_uid: uid,
        sandbox_gid: gid,
        sandbox_username: &username,
        sandbox_home: &sandbox_home,
        debug: std::env::var("SANDSEAL_DEBUG").is_ok(),
        rebuild: args.rebuild,
        agent_args: &args.agent_args,
        settings: &settings,
        tmp_dir: &tmp_path,
        script_dir: &script_dir,
    };

    let override_yaml = compose::generate_compose_override(&compose_ctx)?;
    let override_path = tmp_path.join("docker-compose.yml");
    std::fs::write(&override_path, &override_yaml)?;
    debug!("wrote compose override to {}", override_path.display());

    // Build compose command
    let compose_cmd = runtime::compose_cmd(&instance_name, &script_dir, &override_path);

    // Setup cleanup guard
    let cleanup_hooks = settings.hooks.as_ref()
        .and_then(|h| h.cleanup_host.as_ref())
        .cloned()
        .unwrap_or_default();

    let guard = Arc::new(Mutex::new(CleanupGuard::new(
        compose_cmd.clone(),
        cleanup_hooks,
        project_dir.clone(),
        tmp_path.to_path_buf(),
    )));

    register_signal_handler(Arc::clone(&guard));

    // Compose env vars (needed by base docker-compose.yml template)
    let cachebust = if args.rebuild {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    } else {
        "1".to_string()
    };

    let compose_env = runtime::ComposeEnv {
        project_name: project_name.clone(),
        project_dir: project_dir.to_string_lossy().to_string(),
        sandbox_uid: uid.to_string(),
        sandbox_gid: gid.to_string(),
        sandbox_username: username.clone(),
        sandbox_home: sandbox_home.clone(),
        cachebust,
    };

    // Compose up
    runtime::compose_up(&compose_cmd, args.rebuild, &compose_env)?;

    // Wait for container and attach
    let container_name = runtime::get_container_name(&instance_name)?;
    runtime::wait_and_attach(&container_name)?;

    // Cleanup runs via Drop on guard
    let mut guard = guard.lock().unwrap();
    guard.cleanup();

    Ok(())
}

pub fn destroy(args: DestroyArgs) -> Result<()> {
    if args.all {
        return destroy_all();
    }

    let project_dir = resolve_project_dir(&args.path)?;
    let project_name = create_project_name(&project_dir);

    info!("destroying sandbox for {}", project_dir.display());

    // Find and stop all containers with this project name
    let output = std::process::Command::new("docker")
        .args([
            "ps", "-a",
            "--filter", &format!("label=sandseal.project_name={project_name}"),
            "--format", "{{.Names}}",
        ])
        .output()
        .context("failed to list containers")?;

    let containers: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if containers.is_empty() {
        info!("no sandbox found for {}", project_dir.display());
        return Ok(());
    }

    for name in &containers {
        info!("stopping container: {name}");
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", name])
            .output();
    }

    // Remove cached image
    let image_name = format!("sandseal-sandbox/agent-{project_name}:latest");
    let _ = std::process::Command::new("docker")
        .args(["rmi", &image_name])
        .output();

    info!("sandbox destroyed");
    Ok(())
}

fn destroy_all() -> Result<()> {
    info!("destroying all sandseal sandboxes...");

    let output = std::process::Command::new("docker")
        .args([
            "ps", "-a",
            "--filter", "label=sandseal.project_name",
            "--format", "{{.Names}}",
        ])
        .output()
        .context("failed to list containers")?;

    let containers: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    for name in &containers {
        info!("stopping container: {name}");
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", name])
            .output();
    }

    // Remove all sandseal images
    let output = std::process::Command::new("docker")
        .args(["images", "--format", "{{.Repository}}:{{.Tag}}", "sandseal-sandbox/*"])
        .output()?;

    let images: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    for img in &images {
        let _ = std::process::Command::new("docker")
            .args(["rmi", img])
            .output();
    }

    info!("all sandboxes destroyed");
    Ok(())
}

pub fn status() -> Result<()> {
    let output = std::process::Command::new("docker")
        .args([
            "ps",
            "--filter", "label=sandseal.project_name",
            "--format", "table {{.Names}}\t{{.Status}}\t{{.Label \"sandseal.project_dir\"}}",
        ])
        .output()
        .context("failed to list containers")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        println!("no running sandboxes");
    } else {
        println!("{stdout}");
    }

    Ok(())
}
