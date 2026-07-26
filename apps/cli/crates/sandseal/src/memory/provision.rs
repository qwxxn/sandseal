use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

/// Wires memory into the agent's own configuration. Runs INSIDE the sandbox, from the
/// entrypoint, because the agent home is a Docker volume the host does not write into.
///
/// Both files may already contain the user's own settings — a mounted ~/.claude.json is how
/// people carry their login and MCP servers into the sandbox — so every write merges and is
/// idempotent. Overwriting either file would silently discard their configuration.

const SERVER_NAME: &str = "sandseal-memory";
const RECALL_COMMAND: &str = "sandseal memory recall --stdin";

pub fn run(home: Option<PathBuf>) -> Result<()> {
    let home = match home.or_else(dirs::home_dir) {
        Some(home) => home,
        None => anyhow::bail!("cannot determine home directory"),
    };

    let mcp_changed = provision_mcp_server(&home.join(".claude.json"))?;
    let hook_changed = provision_recall_hook(&home.join(".claude/settings.json"))?;

    // Quiet when there was nothing to do: this runs on every start, and a line of output per
    // start would be noise in the agent's terminal.
    if mcp_changed || hook_changed {
        println!("Sandseal memory: registered MCP server and recall hook.");
    }
    Ok(())
}

/// Claude Code reads user-level MCP servers from `~/.claude.json` only; a `.claude/.mcp.json`
/// is silently ignored, which makes a wrong guess here look like a broken bridge.
fn provision_mcp_server(path: &Path) -> Result<bool> {
    let mut root = read_object(path)?;

    let desired = json!({
        "type": "stdio",
        "command": "sandseal",
        "args": ["memory", "mcp"],
    });

    let servers = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        anyhow::bail!("mcpServers in {} is not an object", path.display());
    };

    if servers.get(SERVER_NAME) == Some(&desired) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), desired);

    write_object(path, &root)?;
    Ok(true)
}

/// UserPromptSubmit hook for automatic recall. Appended to whatever hooks already exist.
fn provision_recall_hook(path: &Path) -> Result<bool> {
    let mut root = read_object(path)?;

    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        anyhow::bail!("hooks in {} is not an object", path.display());
    };

    let event = hooks
        .entry("UserPromptSubmit".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = event.as_array_mut() else {
        anyhow::bail!("hooks.UserPromptSubmit in {} is not an array", path.display());
    };

    if groups.iter().any(contains_recall_command) {
        return Ok(false);
    }

    groups.push(json!({
        "hooks": [{ "type": "command", "command": RECALL_COMMAND }]
    }));

    write_object(path, &root)?;
    Ok(true)
}

fn contains_recall_command(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("command").and_then(Value::as_str) == Some(RECALL_COMMAND)
            })
        })
}

/// Reads a JSON object, treating a missing file as empty. A file that exists but does not
/// parse is an error: silently replacing it would destroy the user's configuration.
fn read_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }

    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON — refusing to overwrite it", path.display()))?;

    match value {
        Value::Object(map) => Ok(map),
        _ => anyhow::bail!("{} does not contain a JSON object", path.display()),
    }
}

fn write_object(path: &Path, root: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write-then-rename: a killed process must not leave a half-written ~/.claude.json,
    // which would cost the user their login and MCP servers.
    let tmp = path.with_extension("sandseal-tmp");
    std::fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(root)?))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn registers_the_server_and_the_hook_from_nothing() {
        let home = tempfile::tempdir().unwrap();
        run(Some(home.path().to_path_buf())).unwrap();

        let claude = read(&home.path().join(".claude.json"));
        assert_eq!(claude["mcpServers"][SERVER_NAME]["command"], "sandseal");
        assert_eq!(claude["mcpServers"][SERVER_NAME]["args"][0], "memory");

        let settings = read(&home.path().join(".claude/settings.json"));
        assert_eq!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            RECALL_COMMAND
        );
    }

    #[test]
    fn is_idempotent_across_starts() {
        let home = tempfile::tempdir().unwrap();
        run(Some(home.path().to_path_buf())).unwrap();
        run(Some(home.path().to_path_buf())).unwrap();
        run(Some(home.path().to_path_buf())).unwrap();

        let settings = read(&home.path().join(".claude/settings.json"));
        assert_eq!(
            settings["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            1,
            "the recall hook must not stack up once per start"
        );
    }

    #[test]
    fn keeps_the_users_own_mcp_servers_and_login() {
        let home = tempfile::tempdir().unwrap();
        let claude_json = home.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            r#"{"userID":"abc123","mcpServers":{"other":{"command":"something"}}}"#,
        )
        .unwrap();

        run(Some(home.path().to_path_buf())).unwrap();

        let claude = read(&claude_json);
        assert_eq!(claude["userID"], "abc123", "must not drop unrelated keys");
        assert_eq!(claude["mcpServers"]["other"]["command"], "something");
        assert!(claude["mcpServers"][SERVER_NAME].is_object());
    }

    #[test]
    fn appends_to_existing_hooks_instead_of_replacing_them() {
        let home = tempfile::tempdir().unwrap();
        let settings = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"model":"opus","hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"my-own-hook"}]}],"Stop":[]}}"#,
        )
        .unwrap();

        run(Some(home.path().to_path_buf())).unwrap();

        let value = read(&settings);
        assert_eq!(value["model"], "opus");
        assert!(value["hooks"]["Stop"].is_array());
        let groups = value["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "the user's hook must survive");
        assert_eq!(groups[0]["hooks"][0]["command"], "my-own-hook");
    }

    #[test]
    fn refuses_to_overwrite_a_file_it_cannot_parse() {
        let home = tempfile::tempdir().unwrap();
        let claude_json = home.path().join(".claude.json");
        std::fs::write(&claude_json, "{ this is not json").unwrap();

        let err = run(Some(home.path().to_path_buf())).unwrap_err();
        assert!(format!("{err:#}").contains("refusing to overwrite"));
        // The original bytes must still be there.
        assert_eq!(std::fs::read_to_string(&claude_json).unwrap(), "{ this is not json");
    }

    #[test]
    fn treats_an_empty_file_as_empty_config() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), "   \n").unwrap();

        run(Some(home.path().to_path_buf())).unwrap();

        let claude = read(&home.path().join(".claude.json"));
        assert!(claude["mcpServers"][SERVER_NAME].is_object());
    }

    #[test]
    fn leaves_no_temporary_file_behind() {
        let home = tempfile::tempdir().unwrap();
        run(Some(home.path().to_path_buf())).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("sandseal-tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
