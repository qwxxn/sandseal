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
    pub image: &'a str,
    pub sandbox_home: &'a str,
    pub debug: bool,
    pub agent_args: &'a [String],
    pub settings: &'a Settings,
    pub tmp_dir: &'a Path,
    pub script_dir: &'a Path,
    /// Memory credential for this session, when the account has memory. None = no memory.
    pub memory: Option<&'a crate::memory::session::MemorySession>,
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

    // Persistent agent home volume (survives container restarts — keeps CLI logins, installed tools, etc.)
    volumes.push(format!("sandseal-agent-home:{}", ctx.sandbox_home));

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

    // Sandseal settings only (not auth.json or identity.key), READ-ONLY.
    // The agent may need to read the machine-wide defaults to explain its own environment,
    // but it must not rewrite them: a writable mount lets a sandboxed agent drop
    // files.exclude or set docker.passthrough for every FUTURE session on this machine.
    let home = dirs::home_dir().unwrap_or_default();
    let settings_file = home.join(".sandseal/settings.json");
    if settings_file.is_file() {
        volumes.push(format!(
            "{}:{}/.sandseal/settings.json:ro",
            settings_file.display(),
            ctx.sandbox_home
        ));
    }

    // Bundled skills (read-only). The entrypoint copies them into the agent home so the
    // agent can learn how to configure the sandbox it is running in.
    let skills_dir = ctx.script_dir.join("agents/skills");
    if skills_dir.is_dir() {
        volumes.push(format!("{}:/opt/sandseal/skills:ro", skills_dir.display()));
    }

    // Agent config that switches memory on, passed via --mcp-config/--settings rather than
    // written into the agent's own files: those are commonly bind-mounted from the host, so
    // writing them either fails (EBUSY on a mount point) or leaks the sandbox's hook into
    // every session on the machine.
    let memory_config = ctx.tmp_dir.join("memory");
    if ctx.memory.is_some() && memory_config.is_dir() {
        volumes.push(format!(
            "{}:{}:ro",
            crate::memory::provision::host_mcp_config(ctx.tmp_dir).display(),
            crate::memory::provision::MCP_CONFIG_PATH
        ));
        volumes.push(format!(
            "{}:{}:ro",
            crate::memory::provision::host_settings(ctx.tmp_dir).display(),
            crate::memory::provision::SETTINGS_PATH
        ));
    }

    // The memory bridge is this same binary, so mount it in. Read-only: the agent has no
    // business rewriting the tool that carries its credential.
    if ctx.memory.is_some() {
        match std::env::current_exe() {
            Ok(exe) => volumes.push(format!("{}:/usr/local/bin/sandseal:ro", exe.display())),
            Err(err) => tracing::warn!("cannot locate own binary, memory bridge unavailable: {err}"),
        }
    }
    // Prestart scripts
    let prestart_dir = ctx.tmp_dir.join("prestart-scripts");
    if prestart_dir.is_dir() {
        volumes.push(format!("{}:/tmp/prestart-scripts:ro", prestart_dir.display()));
    }

    // Environment
    let mut environment = HashMap::new();

    // Memory: the session credential and the backend it belongs to. Deliberately the only
    // memory configuration in the container — there is no endpoint or key in a file for a
    // prompt injection to point somewhere else.
    if let Some(memory) = ctx.memory {
        environment.insert("SANDSEAL_MEMORY_TOKEN".to_string(), memory.token.clone());
        environment.insert("SANDSEAL_API_URL".to_string(), memory.api_url.clone());
        // Always written, never omitted: the default lives in one place, and a session whose
        // scope you have to infer from an absent variable is one nobody can debug.
        let cross_project = ctx
            .settings
            .memory
            .as_ref()
            .and_then(|m| m.cross_project())
            .unwrap_or(true);
        environment.insert(
            "SANDSEAL_MEMORY_CROSS_PROJECT".to_string(),
            if cross_project { "1" } else { "0" }.to_string(),
        );
    }

    // Runtime package log path (for auto-suggest after exit)
    let pkg_log = format!("{}/.sandseal/.runtime-packages", ctx.project_dir.display());
    environment.insert("SANDSEAL_RUNTIME_PACKAGES".to_string(), pkg_log);

    if let Some(env_vars) = &ctx.settings.environment {
        for (key, val) in env_vars {
            let expanded = crate::path::resolve::expand_env_vars(val);
            environment.insert(key.clone(), expanded);
        }
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
        build_agent_command(ctx.script_dir, ctx.agent_args, ctx.memory.is_some())?
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
        &labels,
        &command,
        host_network,
        &extra_hosts,
    );

    Ok(yaml)
}

fn build_agent_command(script_dir: &Path, agent_args: &[String], memory: bool) -> Result<Vec<String>> {
    let command_file = script_dir.join("agents/claude/command.json");
    let content = std::fs::read_to_string(&command_file)?;
    let mut cmd: Vec<String> = serde_json::from_str(&content)?;
    if memory {
        cmd.extend(crate::memory::provision::agent_flags());
    }
    // User arguments last so they can still override anything we added.
    cmd.extend_from_slice(agent_args);
    Ok(cmd)
}

fn format_compose_yaml(
    ctx: &ComposeContext,
    volumes: &[String],
    environment: &HashMap<String, String>,
    labels: &HashMap<&str, String>,
    command: &[String],
    host_network: bool,
    extra_hosts: &[(String, String)],
) -> String {
    let mut yaml = String::from("services:\n  agent:\n");

    // Image (prebuilt by the image module — compose only runs it)
    yaml.push_str(&format!("    image: {}\n", ctx.image));

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

    // Top-level volumes declaration. Pin explicit names so compose does NOT prefix
    // them with the (per-instance, randomized) project name — otherwise every start
    // gets a fresh empty volume and the agent home (CLI logins, installed tools,
    // downloaded browsers) never persists across restarts.
    yaml.push_str("\nvolumes:\n");
    yaml.push_str("  sandseal-agent-home:\n");
    yaml.push_str("    name: sandseal-sandbox-agent-home\n");
    yaml.push_str("    external: false\n");
    yaml.push_str("  sandseal-apt-cache:\n");
    yaml.push_str("    name: sandseal-sandbox-apt-cache\n");
    yaml.push_str("    external: false\n");

    yaml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::Settings;

    /// generate_compose_override reads the agent command file, so every fixture needs it.
    fn script_dir_with_agent() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents/claude")).unwrap();
        std::fs::write(
            dir.path().join("agents/claude/command.json"),
            r#"["claude", "--dangerously-skip-permissions"]"#,
        )
        .unwrap();
        dir
    }

    fn context<'a>(script_dir: &'a Path, project_dir: &'a Path, settings: &'a Settings) -> ComposeContext<'a> {
        ComposeContext {
            project_dir,
            project_name: "demo",
            instance_name: "agent",
            image: "sandseal-sandbox/agent-claude:test",
            sandbox_home: "/home/agent",
            debug: false,
            agent_args: &[],
            settings,
            tmp_dir: project_dir,
            script_dir,
            memory: None,
        }
    }

    #[test]
    fn bundled_skills_are_mounted_read_only_when_present() {
        let script_dir = script_dir_with_agent();
        std::fs::create_dir_all(script_dir.path().join("agents/skills/sandseal")).unwrap();
        std::fs::write(script_dir.path().join("agents/skills/sandseal/SKILL.md"), "---\nname: sandseal\n---\n").unwrap();

        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let yaml = generate_compose_override(&context(script_dir.path(), project.path(), &settings)).unwrap();

        assert!(
            yaml.contains("/opt/sandseal/skills:ro"),
            "skills must reach the container so the agent can learn to configure it:\n{yaml}"
        );
    }

    #[test]
    fn no_skills_mount_when_the_directory_is_absent() {
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let yaml = generate_compose_override(&context(script_dir.path(), project.path(), &settings)).unwrap();

        assert!(!yaml.contains("/opt/sandseal/skills"));
    }

    #[test]
    fn memory_credential_and_bridge_binary_reach_the_container() {
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let session = crate::memory::session::MemorySession {
            id: "sess_1".to_string(),
            token: "tok_abc".to_string(),
            api_url: "https://sandseal.io".to_string(),
        };

        let mut ctx = context(script_dir.path(), project.path(), &settings);
        ctx.memory = Some(&session);
        let yaml = generate_compose_override(&ctx).unwrap();

        assert!(yaml.contains("SANDSEAL_MEMORY_TOKEN: \"tok_abc\""), "{yaml}");
        assert!(yaml.contains("SANDSEAL_API_URL: \"https://sandseal.io\""), "{yaml}");
        assert!(yaml.contains("/usr/local/bin/sandseal:ro"), "the bridge binary must be mounted:\n{yaml}");
    }

    #[test]
    fn recall_spans_the_space_unless_the_settings_narrow_it() {
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let session = crate::memory::session::MemorySession {
            id: "sess_1".to_string(),
            token: "tok".to_string(),
            api_url: "https://sandseal.io".to_string(),
        };

        let default = Settings::default();
        let mut ctx = context(script_dir.path(), project.path(), &default);
        ctx.memory = Some(&session);
        let yaml = generate_compose_override(&ctx).unwrap();
        assert!(yaml.contains("SANDSEAL_MEMORY_CROSS_PROJECT: \"1\""), "{yaml}");

        let narrowed = Settings {
            memory: Some(crate::config::schema::MemorySettings {
                scope: Some(crate::config::schema::MemoryScopeSettings {
                    project: None,
                    cross_project: Some(false),
                }),
            }),
            ..Settings::default()
        };
        let mut ctx = context(script_dir.path(), project.path(), &narrowed);
        ctx.memory = Some(&session);
        let yaml = generate_compose_override(&ctx).unwrap();
        assert!(yaml.contains("SANDSEAL_MEMORY_CROSS_PROJECT: \"0\""), "{yaml}");
    }

    #[test]
    fn no_memory_means_no_scope_variable() {
        // Without a credential the variable would describe a session that does not exist.
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let yaml = generate_compose_override(&context(script_dir.path(), project.path(), &settings)).unwrap();

        assert!(!yaml.contains("SANDSEAL_MEMORY_CROSS_PROJECT"), "{yaml}");
    }

    #[test]
    fn the_agent_is_told_to_load_the_memory_config() {
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let session = crate::memory::session::MemorySession {
            id: "sess_1".to_string(),
            token: "tok".to_string(),
            api_url: "https://sandseal.io".to_string(),
        };
        crate::memory::provision::render(project.path()).unwrap();

        let mut ctx = context(script_dir.path(), project.path(), &settings);
        ctx.memory = Some(&session);
        let yaml = generate_compose_override(&ctx).unwrap();

        // Config travels on the command line: the agent's own files are often bind-mounted
        // from the host, where writing either fails or leaks into every session on the box.
        assert!(yaml.contains("--mcp-config"), "{yaml}");
        assert!(yaml.contains(crate::memory::provision::MCP_CONFIG_PATH), "{yaml}");
        assert!(yaml.contains("--settings"), "{yaml}");
        assert!(yaml.contains(crate::memory::provision::SETTINGS_PATH), "{yaml}");
    }

    #[test]
    fn nothing_memory_related_leaks_in_when_the_session_has_no_memory() {
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let yaml = generate_compose_override(&context(script_dir.path(), project.path(), &settings)).unwrap();

        assert!(!yaml.contains("SANDSEAL_MEMORY_TOKEN"));
        assert!(!yaml.contains("/usr/local/bin/sandseal"));
        assert!(!yaml.contains("--mcp-config"));
    }

    #[test]
    fn the_machine_wide_settings_mount_is_read_only() {
        // A writable mount would let a sandboxed agent drop files.exclude or enable
        // docker.passthrough for every future session on this machine.
        let script_dir = script_dir_with_agent();
        let project = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let yaml = generate_compose_override(&context(script_dir.path(), project.path(), &settings)).unwrap();

        for line in yaml.lines() {
            if line.contains("/.sandseal/settings.json") {
                // The YAML entry is quoted, so match the suffix inside the quotes.
                assert!(
                    line.trim_end().trim_end_matches('"').ends_with(":ro"),
                    "settings mount must be read-only: {line}"
                );
            }
        }
    }
}
