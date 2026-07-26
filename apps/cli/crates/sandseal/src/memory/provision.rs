use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

/// Renders the agent configuration that turns memory on, as files passed to the agent on its
/// command line — `--mcp-config` and `--settings`.
///
/// The first version of this wrote into `~/.claude.json` and `~/.claude/settings.json` from
/// inside the container. Two things killed that approach, both found on a real machine:
///
/// - people bind-mount those paths in to carry a login, and you cannot rename over a bind
///   mount — the write failed with EBUSY and provisioning silently gave up
/// - worse, when the write DOES succeed it lands on the host's real files, so a sandbox
///   would have installed its recall hook into every Claude Code session on the machine
///
/// Passing files on the command line has neither problem, needs no write inside the
/// container at all, and is idempotent by construction. Both flags merge with the user's own
/// configuration rather than replacing it.
pub const MCP_CONFIG_PATH: &str = "/run/sandseal/mcp.json";
pub const SETTINGS_PATH: &str = "/run/sandseal/settings.json";

const SERVER_NAME: &str = "sandseal-memory";
const RECALL_COMMAND: &str = "sandseal memory recall --stdin";

/// Host-side locations, so the renderer and the compose mounts cannot disagree.
pub fn host_mcp_config(tmp_dir: &Path) -> PathBuf {
    tmp_dir.join("memory/mcp.json")
}

pub fn host_settings(tmp_dir: &Path) -> PathBuf {
    tmp_dir.join("memory/settings.json")
}

/// Writes both files into the instance tmp directory on the host.
pub fn render(tmp_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(tmp_dir.join("memory")).context("failed to create memory config dir")?;

    std::fs::write(
        host_mcp_config(tmp_dir),
        format!("{}\n", serde_json::to_string_pretty(&mcp_config_json())?),
    )?;
    std::fs::write(
        host_settings(tmp_dir),
        format!("{}\n", serde_json::to_string_pretty(&settings_json())?),
    )?;

    Ok(())
}

fn mcp_config_json() -> serde_json::Value {
    json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": "sandseal",
                "args": ["memory", "mcp"],
            }
        }
    })
}

fn settings_json() -> serde_json::Value {
    json!({
        "hooks": {
            "UserPromptSubmit": [
                { "hooks": [{ "type": "command", "command": RECALL_COMMAND }] }
            ]
        }
    })
}

/// Flags appended to the agent command when memory is enabled. Not `--strict-mcp-config`:
/// the user's own MCP servers must keep working alongside this one.
pub fn agent_flags() -> Vec<String> {
    vec![
        "--mcp-config".to_string(),
        MCP_CONFIG_PATH.to_string(),
        "--settings".to_string(),
        SETTINGS_PATH.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_both_files_into_the_instance_tmp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        render(tmp.path()).unwrap();

        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(host_mcp_config(tmp.path())).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"][SERVER_NAME]["command"], "sandseal");
        assert_eq!(mcp["mcpServers"][SERVER_NAME]["args"][0], "memory");

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(host_settings(tmp.path())).unwrap()).unwrap();
        assert_eq!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            RECALL_COMMAND
        );
    }

    #[test]
    fn rendering_twice_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        render(tmp.path()).unwrap();
        let first = std::fs::read_to_string(host_mcp_config(tmp.path())).unwrap();
        render(tmp.path()).unwrap();
        let second = std::fs::read_to_string(host_mcp_config(tmp.path())).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn flags_point_at_the_mounted_paths_and_do_not_go_strict() {
        let flags = agent_flags();
        assert!(flags.contains(&MCP_CONFIG_PATH.to_string()));
        assert!(flags.contains(&SETTINGS_PATH.to_string()));
        // --strict-mcp-config would hide the user's own servers.
        assert!(!flags.iter().any(|f| f.contains("strict")));
    }
}
