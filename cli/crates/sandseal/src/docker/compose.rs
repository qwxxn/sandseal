use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::config::Settings;
use crate::path::exclusion::resolve_exclusions;
use crate::path::inclusion::resolve_inclusions;
use crate::path::resolve::resolve_host_path;

pub struct ComposeContext<'a> {
    pub project_dir: &'a Path,
    pub project_name: &'a str,
    pub instance_name: &'a str,
    pub sandbox_uid: u32,
    pub sandbox_gid: u32,
    pub sandbox_username: &'a str,
    pub sandbox_home: &'a str,
    pub debug: bool,
    pub rebuild: bool,
    pub agent_args: &'a [String],
    pub settings: &'a Settings,
    pub tmp_dir: &'a Path,
    pub script_dir: &'a Path,
}

/// Generate docker-compose override YAML for a sandbox instance.
pub fn generate_compose_override(ctx: &ComposeContext) -> Result<String> {
    let mut volumes = Vec::new();

    // Project directory (read-write)
    volumes.push(format!("{}:{}", ctx.project_dir.display(), ctx.project_dir.display()));

    // File exclusions
    if let Some(files) = &ctx.settings.files {
        if let Some(excludes) = &files.exclude {
            let mounts = resolve_exclusions(excludes, ctx.project_dir)?;
            for mount in &mounts {
                volumes.push(mount.to_volume_mount(ctx.tmp_dir));
            }
        }

        // File inclusions
        if let Some(includes) = &files.include {
            let mounts = resolve_inclusions(includes, ctx.project_dir, ctx.sandbox_home);
            for mount in &mounts {
                volumes.push(mount.to_volume_mount());
            }
        }
    }

    // Workspace
    if let Some(workspace) = &ctx.settings.workspace {
        let ws_path = resolve_host_path(&workspace.dir, ctx.project_dir);
        if ws_path.is_dir() {
            let mode = if workspace.readwrite { "" } else { ":ro" };
            volumes.push(format!("{path}:{path}{mode}", path = ws_path.display()));
        } else {
            tracing::warn!("workspace directory does not exist: {}", ws_path.display());
        }
    }

    // Shared apt cache volume (speeds up repeated installs across sandboxes)
    volumes.push("sandseal-apt-cache:/var/cache/apt".to_string());

    // Docker socket (opt-in via settings)
    let docker_passthrough = ctx.settings.docker.as_ref()
        .and_then(|d| d.passthrough)
        .unwrap_or(false);
    if docker_passthrough {
        tracing::warn!("docker.passthrough is enabled — sandbox has full Docker access");
        volumes.push("/var/run/docker.sock:/var/run/docker.sock".to_string());
    }

    // Sandseal settings only (not auth.json or identity.key)
    let home = dirs::home_dir().unwrap_or_default();
    let settings_file = home.join(".sandseal/settings.json");
    if settings_file.is_file() {
        volumes.push(format!(
            "{}:{}/.sandseal/settings.json",
            settings_file.display(),
            ctx.sandbox_home
        ));
    }
    // Prestart scripts
    let prestart_dir = ctx.tmp_dir.join("prestart-scripts");
    if prestart_dir.is_dir() {
        volumes.push(format!("{}:/tmp/prestart-scripts:ro", prestart_dir.display()));
    }

    // Environment
    let mut environment = HashMap::new();

    // Runtime package log path (for auto-suggest after exit)
    let pkg_log = format!("{}/.sandseal/.runtime-packages", ctx.project_dir.display());
    environment.insert("SANDSEAL_RUNTIME_PACKAGES".to_string(), pkg_log);

    if let Some(env_vars) = &ctx.settings.environment {
        for (key, val) in env_vars {
            let expanded = crate::path::resolve::expand_env_vars(val);
            environment.insert(key.clone(), expanded);
        }
    }

    // Build args
    let mut build_args = HashMap::new();
    build_args.insert("AGENT_USERNAME", ctx.sandbox_username.to_string());
    build_args.insert("AGENT_HOME", ctx.sandbox_home.to_string());
    build_args.insert("UID", ctx.sandbox_uid.to_string());
    build_args.insert("GID", ctx.sandbox_gid.to_string());

    if let Some(deps) = &ctx.settings.dependencies {
        if !deps.is_empty() {
            build_args.insert("EXTRA_PACKAGES", deps.join(" "));
        }
    }

    if let Some(container) = &ctx.settings.container {
        if let Some(base) = &container.base_image {
            build_args.insert("BASE_IMAGE", base.clone());
        }
    }

    if ctx.rebuild {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        build_args.insert("CACHEBUST", timestamp.to_string());
    }

    // Labels
    let mut labels = HashMap::new();
    labels.insert("sandseal.project_name", ctx.project_name.to_string());
    labels.insert("sandseal.project_dir", ctx.project_dir.to_string_lossy().to_string());
    labels.insert("sandseal.instance_name", ctx.instance_name.to_string());

    // Command
    let command = if ctx.debug {
        vec!["bash".to_string()]
    } else {
        build_agent_command(ctx.script_dir, ctx.agent_args)?
    };

    // Network mode
    let host_network = ctx.settings.network.as_ref()
        .and_then(|n| n.mode.as_deref())
        .map(|m| m == "host")
        .unwrap_or(false);
    if host_network {
        tracing::warn!("network.mode is 'host' — sandbox shares host network namespace");
    }

    // Service endpoints → extra_hosts
    let extra_hosts: Vec<(String, String)> = ctx.settings.network.as_ref()
        .and_then(|n| n.services.as_ref())
        .map(|services| {
            services.iter().map(|(hostname, target)| {
                // Strip port if present (extra_hosts only does hostname → IP)
                let host = target.split(':').next().unwrap_or(target);
                (hostname.clone(), host.to_string())
            }).collect()
        })
        .unwrap_or_default();

    // Build YAML
    let yaml = format_compose_yaml(
        ctx,
        &volumes,
        &environment,
        &build_args,
        &labels,
        &command,
        host_network,
        &extra_hosts,
    );

    Ok(yaml)
}

fn build_agent_command(script_dir: &Path, agent_args: &[String]) -> Result<Vec<String>> {
    let command_file = script_dir.join("agents/claude/command.json");
    let content = std::fs::read_to_string(&command_file)?;
    let mut cmd: Vec<String> = serde_json::from_str(&content)?;
    cmd.extend_from_slice(agent_args);
    Ok(cmd)
}

fn format_compose_yaml(
    ctx: &ComposeContext,
    volumes: &[String],
    environment: &HashMap<String, String>,
    build_args: &HashMap<&str, String>,
    labels: &HashMap<&str, String>,
    command: &[String],
    host_network: bool,
    extra_hosts: &[(String, String)],
) -> String {
    let mut yaml = String::from("services:\n  agent:\n");

    // Image
    yaml.push_str(&format!(
        "    image: sandseal-sandbox/agent-{}:latest\n",
        ctx.project_name
    ));

    // Build (context = tmp_dir so COPY finds agent-installs/, entrypoint.sh etc.)
    yaml.push_str(&format!(
        "    build:\n      context: \"{}\"\n      dockerfile: \"{}/agents/Dockerfile\"\n",
        ctx.tmp_dir.display(),
        ctx.script_dir.display(),
    ));
    if !build_args.is_empty() {
        yaml.push_str("      args:\n");
        for (key, val) in build_args {
            yaml.push_str(&format!("        {key}: \"{val}\"\n"));
        }
    }

    // Network mode
    if host_network {
        yaml.push_str("    network_mode: host\n");
    }

    // Extra hosts (network.services)
    if !extra_hosts.is_empty() {
        yaml.push_str("    extra_hosts:\n");
        for (hostname, target) in extra_hosts {
            yaml.push_str(&format!("      - \"{hostname}:{target}\"\n"));
        }
    }

    // Volumes
    if !volumes.is_empty() {
        yaml.push_str("    volumes:\n");
        for vol in volumes {
            yaml.push_str(&format!("      - \"{vol}\"\n"));
        }
    }

    // Environment
    if !environment.is_empty() {
        yaml.push_str("    environment:\n");
        for (key, val) in environment {
            yaml.push_str(&format!("      {key}: \"{val}\"\n"));
        }
    }

    // Labels
    if !labels.is_empty() {
        yaml.push_str("    labels:\n");
        for (key, val) in labels {
            yaml.push_str(&format!("      {key}: \"{val}\"\n"));
        }
    }

    // Memory limits
    if let Some(container) = &ctx.settings.container {
        if let Some(mem) = &container.memory_limit {
            yaml.push_str(&format!("    mem_limit: {mem}\n"));
        }
        if let Some(memswap) = &container.memory_swap_limit {
            yaml.push_str(&format!("    memswap_limit: {memswap}\n"));
        }
    }

    // Command
    if !command.is_empty() {
        let cmd_json = serde_json::to_string(command).unwrap();
        yaml.push_str(&format!("    command: {cmd_json}\n"));
    }

    yaml
}
