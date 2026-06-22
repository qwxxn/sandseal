use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use crate::cli::{BuildArgs, DestroyArgs, StartArgs};
use crate::config::merge::deep_merge;
use crate::config::validate::validate_settings;
use crate::config::Settings;
use crate::docker::{build, compose, image, runtime};
use crate::sandbox::cleanup::{register_signal_handler, CleanupGuard};
use crate::sandbox::hooks;

/// The agent CLI baked into the sandbox image. Only Claude Code is supported today.
const AGENT: &str = "claude";

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
    format!("{}-{}", sanitize_basename(project_dir), &hash[..8])
}

/// Docker-safe lowercase basename of the project directory.
fn sanitize_basename(project_dir: &Path) -> String {
    project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
}

/// Load and deep-merge global + project settings.
fn load_merged_settings(project_dir: &Path, home: &Path) -> Result<Settings> {
    let global = home.join(".sandseal/settings.json");
    let project = project_dir.join(".sandseal/settings.json");
    let mut merged = serde_json::json!({});

    for path in [&global, &project] {
        if path.exists() {
            let validated = validate_settings(path)?;
            merged = deep_merge(&merged, &validated);
            debug!("merged settings from {}", path.display());
        }
    }

    serde_json::from_value(merged).context("failed to deserialize merged settings")
}

/// Resolve the setup hook script to an existing host path, if configured.
fn resolve_setup_script(settings: &Settings, project_dir: &Path) -> Option<PathBuf> {
    settings.hooks.as_ref()
        .and_then(|h| h.setup.as_ref())
        .and_then(|s| s.script.as_ref())
        .map(|s| crate::path::resolve::resolve_host_path(s, project_dir))
        .filter(|p| {
            if p.exists() {
                true
            } else {
                tracing::warn!("setup script not found: {}", p.display());
                false
            }
        })
}

fn resolve_base_image(settings: &Settings) -> String {
    settings.container.as_ref()
        .and_then(|c| c.base_image.clone())
        .unwrap_or_else(|| "ubuntu:24.04".to_string())
}

/// Build the image spec shared by `start` and `build`.
fn image_spec<'a>(
    project_basename: &'a str,
    base_image: &'a str,
    dependencies: &'a [String],
    setup_script: Option<&'a Path>,
    script_dir: &'a Path,
    uid: u32,
    gid: u32,
    username: &'a str,
    home: &'a str,
    rebuild: bool,
) -> image::ImageSpec<'a> {
    image::ImageSpec {
        agent: AGENT,
        project_basename,
        base_image,
        uid,
        gid,
        username,
        home,
        dependencies,
        setup_script,
        script_dir,
        rebuild,
    }
}

fn generate_instance_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 3] = rng.random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Find the sandseal script/assets directory.
fn find_script_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("SANDSEAL_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().unwrap();

    let home_dir = dirs::home_dir().unwrap_or_default();

    for candidate in &[
        home_dir.join(".sandseal"),           // installed: ~/.sandseal/agents/
        exe_dir.join("../../.."),             // cargo run: target/debug -> project root
        exe_dir.to_path_buf(),               // installed: same dir as binary
        exe_dir.join(".."),                   // installed: parent of binary
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

pub struct StartedSandbox {
    pub container_name: String,
    pub project_name: String,
    pub project_dir: PathBuf,
    pub guard: Arc<Mutex<CleanupGuard>>,
}

pub async fn start(args: StartArgs) -> Result<()> {
    let started = prepare_and_launch(&args)?;

    // Local mode: attach interactively
    runtime::wait_and_attach(&started.container_name).await?;

    suggest_runtime_packages(&started.project_dir, &crate::config::Settings::default());

    let mut guard = started.guard.lock().unwrap();
    guard.cleanup();

    Ok(())
}

pub fn start_remote(args: &StartArgs) -> Result<StartedSandbox> {
    let started = prepare_and_launch(args)?;

    // Wait for container to be running (non-interactive)
    let max_retries = 20;
    let delay = std::time::Duration::from_millis(500);
    for attempt in 1..=max_retries {
        let output = std::process::Command::new("docker")
            .args(["inspect", "--format", "{{.State.Status}}", &started.container_name])
            .output()?;
        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("container state (attempt {attempt}/{max_retries}): {state}");
        match state.as_str() {
            "running" => return Ok(started),
            "exited" | "dead" => bail!("container exited before remote bridge"),
            _ => std::thread::sleep(delay),
        }
    }
    bail!("container did not start in time");
}

fn prepare_and_launch(args: &StartArgs) -> Result<StartedSandbox> {
    let project_dir = resolve_project_dir(&args.path)?;
    let project_name = create_project_name(&project_dir);
    let project_basename = sanitize_basename(&project_dir);
    let instance_id = generate_instance_id();
    let instance_name = format!("sandseal-sandbox-{project_name}-{instance_id}");
    let script_dir = find_script_dir()?;

    info!("starting sandbox for {}", project_dir.display());
    debug!("project_name={project_name} instance_name={instance_name}");

    let home = dirs::home_dir().context("cannot determine home directory")?;

    // Create tmp dir (holds the compose override + mounted prestart scripts)
    let tmp_base = home.join(".sandseal/tmp");
    std::fs::create_dir_all(&tmp_base)?;
    let tmp_dir = tempfile::tempdir_in(&tmp_base)?;
    let tmp_path = tmp_dir.keep();

    // Load and merge settings
    let settings = load_merged_settings(&project_dir, &home)?;

    // Run setupHost hooks
    if let Some(hooks_cfg) = &settings.hooks {
        if let Some(setup_host) = &hooks_cfg.setup_host {
            hooks::run_setup_host_hooks(setup_host, &project_dir)?;
        }
    }

    // Resolve hooks
    let setup_script = resolve_setup_script(&settings, &project_dir);

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

    // Copy prestart scripts into the instance tmp dir (mounted into the container)
    build::copy_prestart_scripts(
        &tmp_path,
        &prestart_scripts.iter().map(|(i, p)| (*i, p.as_path())).collect::<Vec<_>>(),
    )?;

    // System info
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    let username = std::env::var("USER").unwrap_or_else(|_| "agent".to_string());
    let sandbox_home = home.to_string_lossy().to_string();

    // Build (or reuse) the sandbox image — shared base + optional per-project overlay
    let dependencies = settings.dependencies.clone().unwrap_or_default();
    let base_image = resolve_base_image(&settings);
    let image = image::ensure_images(&image_spec(
        &project_basename,
        &base_image,
        &dependencies,
        setup_script.as_deref(),
        &script_dir,
        uid,
        gid,
        &username,
        &sandbox_home,
        args.rebuild,
    ))?;

    // Generate compose override
    let compose_ctx = compose::ComposeContext {
        project_dir: &project_dir,
        project_name: &project_name,
        instance_name: &instance_name,
        image: &image,
        sandbox_home: &sandbox_home,
        debug: std::env::var("SANDSEAL_DEBUG").is_ok(),
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

    // Compose up (images already built by the image module)
    let compose_env = runtime::ComposeEnv {
        project_dir: project_dir.to_string_lossy().to_string(),
    };
    runtime::compose_up(&compose_cmd, &compose_env)?;

    let container_name = runtime::get_container_name(&instance_name)?;

    Ok(StartedSandbox {
        container_name,
        project_name,
        project_dir,
        guard,
    })
}

/// Build (or rebuild) the sandbox image for a project without starting a container.
pub fn build(args: BuildArgs) -> Result<()> {
    let project_dir = resolve_project_dir(&args.path)?;
    let project_basename = sanitize_basename(&project_dir);
    let script_dir = find_script_dir()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;

    let settings = load_merged_settings(&project_dir, &home)?;

    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    let username = std::env::var("USER").unwrap_or_else(|_| "agent".to_string());
    let sandbox_home = home.to_string_lossy().to_string();

    let dependencies = settings.dependencies.clone().unwrap_or_default();
    let base_image = resolve_base_image(&settings);
    let setup_script = resolve_setup_script(&settings, &project_dir);

    info!("building sandbox image for {}", project_dir.display());
    let image = image::ensure_images(&image_spec(
        &project_basename,
        &base_image,
        &dependencies,
        setup_script.as_deref(),
        &script_dir,
        uid,
        gid,
        &username,
        &sandbox_home,
        true,
    ))?;

    println!("  Built image: {image}");
    Ok(())
}

fn suggest_runtime_packages(project_dir: &Path, settings: &crate::config::Settings) {
    let log_path = project_dir.join(".sandseal/.runtime-packages");
    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return,
    };

    let existing: Vec<&str> = settings.dependencies.as_ref()
        .map(|d| d.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let new_packages: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !existing.contains(l))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if new_packages.is_empty() {
        let _ = std::fs::remove_file(&log_path);
        return;
    }

    println!();
    println!("  Packages installed at runtime (not in settings.json):");
    for pkg in &new_packages {
        println!("    - {pkg}");
    }
    println!();
    println!("  Add to \"dependencies\" in .sandseal/settings.json to pre-install next time.");

    let _ = std::fs::remove_file(&log_path);
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

    // Remove the project's overlay images (the shared base is left intact for other projects)
    image::remove_project_overlays(AGENT, &sanitize_basename(&project_dir));

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
