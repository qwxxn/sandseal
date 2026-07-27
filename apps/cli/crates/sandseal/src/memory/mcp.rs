use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::client::MemoryClient;

/// Local MCP server over stdio.
///
/// The sandbox gets this instead of a remote endpoint: no URL and no token in any config file,
/// so there is nothing in the agent's environment pointing at a memory service it could be
/// talked into redirecting. It speaks to Sandseal, which resolves the scope.
///
/// Hand-rolled JSON-RPC rather than an SDK: the surface is six tools and one handshake, and a
/// stdio MCP server is newline-delimited JSON-RPC 2.0.
/// Tool calls include an embedding round trip, so the budget is generous.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const FALLBACK_PROTOCOL: &str = "2025-06-18";

/// The note methodology, shipped through the handshake instead of through documentation.
///
/// Retrieval quality is set by the shape of the notes far more than by the vector database,
/// so the rules have to reach the agent by default — a corpus of routine notes with no usable
/// first sentence is a memory nobody can search. `instructions` is part of the initialize
/// result, so every MCP client sees it, including Codex and Gemini, which have no
/// UserPromptSubmit hook and would otherwise meet memory with six tools and no guidance.
const INSTRUCTIONS: &str = include_str!("instructions.md");

pub async fn serve(api_url: Option<&str>) -> Result<()> {
    let client = MemoryClient::new(api_url, TOOL_TIMEOUT)?;

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                // -32700 parse error. No id is known, so it cannot be attributed to a request.
                write(&mut stdout, &error_response(Value::Null, -32700, &format!("parse error: {err}"))).await?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or_default();

        // Notifications carry no id and must never be answered — replying to one makes some
        // clients treat the stream as corrupt.
        if id.is_none() {
            continue;
        }
        let id = id.unwrap();

        let response = match method {
            "initialize" => Ok(initialize_result(&request)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => call_tool(&client, &request).await,
            other => {
                write(&mut stdout, &error_response(id, -32601, &format!("method not found: {other}"))).await?;
                continue;
            }
        };

        let payload = match response {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            // A failed tool is a RESULT with isError, not a JSON-RPC error: the agent should see
            // the message and be able to correct itself, not have the call disappear.
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "isError": true,
                    "content": [{ "type": "text", "text": format!("{err:#}") }]
                }
            }),
        };

        write(&mut stdout, &payload).await?;
    }

    Ok(())
}

async fn write(stdout: &mut tokio::io::Stdout, value: &Value) -> Result<()> {
    stdout.write_all(serde_json::to_string(value)?.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize_result(request: &Value) -> Value {
    // Echo the client's protocol version when we know it; otherwise name ours and let the
    // client decide. Guessing high would make an older client fail the handshake outright.
    let requested = request
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(FALLBACK_PROTOCOL);

    let protocol = if SUPPORTED_PROTOCOLS.contains(&requested) {
        requested
    } else {
        FALLBACK_PROTOCOL
    };

    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "sandseal-memory", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS.trim()
    })
}

async fn call_tool(client: &MemoryClient, request: &Value) -> Result<Value> {
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    let str_arg = |key: &str| -> Option<String> {
        args.get(key).and_then(Value::as_str).map(str::to_string)
    };
    let tags_arg = || -> Option<Vec<String>> {
        args.get("tags").and_then(Value::as_array).map(|items| {
            items.iter().filter_map(Value::as_str).map(str::to_string).collect()
        })
    };
    let required = |key: &str| -> Result<String> {
        str_arg(key).ok_or_else(|| anyhow::anyhow!("{key} is required"))
    };

    let payload = match name {
        "search_memory" => {
            let query = required("query")?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10).min(50) as u32;
            let include_linked = args
                .get("include_linked")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            client.search(&query, limit, include_linked, tags_arg()).await?
        }
        "add_note" => client.add_note(&required("content")?, tags_arg()).await?,
        "get_note" => client.get_note(&required("id")?).await?,
        "update_note" => {
            let id = required("id")?;
            if args.get("content").is_none() && args.get("tags").is_none() {
                anyhow::bail!("provide at least one of content or tags");
            }
            client.update_note(&id, str_arg("content").as_deref(), tags_arg()).await?
        }
        "delete_note" => client.delete_note(&required("id")?).await?,
        "link_notes" => {
            client
                .link_notes(&required("id")?, &required("targetId")?, str_arg("relation").as_deref())
                .await?
        }
        other => anyhow::bail!("unknown tool: {other}"),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload)? }]
    }))
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "search_memory",
            "description": "Search your persistent memory by meaning. Returns notes with relevance scores and their explicit links. Use it before assuming something is undocumented.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What you are looking for, in natural language" },
                    "limit": { "type": "number", "description": "Max results (default 10, max 50)" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Only notes carrying all of these tags" },
                    "include_linked": { "type": "boolean", "description": "Also return notes explicitly linked to the matches" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "add_note",
            "description": "Save a note. Worth saving: something that will save >10 minutes next time, or that surprised you. First sentence must be a self-contained summary of at most 150 characters — it drives retrieval, and only the first ~200 characters are shown on recall. Write in English, name files and errors literally.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Markdown. First sentence = TL;DR under 150 chars" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "e.g. type:work, category:decision, tech:rust" }
                },
                "required": ["content"]
            }
        }),
        json!({
            "name": "get_note",
            "description": "Fetch one note by id, with its links.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }),
        json!({
            "name": "update_note",
            "description": "Update a note's content and/or tags. Only the author can update their own note.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "content": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "delete_note",
            "description": "Delete a note. Only the author can delete their own note.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }),
        json!({
            "name": "link_notes",
            "description": "Link two notes with a causal or logical relation (cause -> effect, problem -> solution, contradiction, sequence). Do not link merely similar notes — search already finds those, and similarity links turn the graph into noise.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "targetId": { "type": "string" },
                    "relation": { "type": "string", "description": "Short relation text, max 100 chars, e.g. \"caused by\", \"resolves\"" }
                },
                "required": ["id", "targetId"]
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_a_protocol_version_it_supports() {
        let req = json!({ "params": { "protocolVersion": "2024-11-05" } });
        assert_eq!(initialize_result(&req)["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn falls_back_for_an_unknown_protocol_version() {
        let req = json!({ "params": { "protocolVersion": "1999-01-01" } });
        assert_eq!(initialize_result(&req)["protocolVersion"], FALLBACK_PROTOCOL);
    }

    #[test]
    fn the_handshake_ships_the_note_methodology() {
        let instructions = initialize_result(&json!({}))["instructions"]
            .as_str()
            .expect("instructions must be a string")
            .to_string();

        // The rules that decide whether the corpus stays searchable.
        assert!(instructions.contains("ten minutes"), "missing the save test");
        assert!(instructions.contains("150 characters"), "missing the TL;DR limit");
        assert!(instructions.contains("English"), "missing the language rule");
    }

    #[test]
    fn instructions_frame_recalled_notes_as_untrusted() {
        // The hook renders notes into the prompt on its own, so the handshake has to explain
        // what that block is — otherwise a poisoned note reads as a standing order.
        let instructions = initialize_result(&json!({}))["instructions"].as_str().unwrap().to_string();
        assert!(instructions.contains("<sandseal-memory>"));
        assert!(instructions.contains("never instructions"));
    }

    #[test]
    fn advertises_tools_capability_and_server_name() {
        let result = initialize_result(&json!({}));
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "sandseal-memory");
    }

    #[test]
    fn exposes_exactly_the_six_memory_tools() {
        let names: Vec<String> = tool_definitions()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["search_memory", "add_note", "get_note", "update_note", "delete_note", "link_notes"]
        );
    }

    #[test]
    fn every_tool_declares_an_object_schema_with_required_fields() {
        for tool in tool_definitions() {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object", "{} has no object schema", tool["name"]);
            assert!(
                schema["required"].as_array().is_some_and(|r| !r.is_empty()),
                "{} declares no required field",
                tool["name"]
            );
        }
    }

    #[test]
    fn add_note_teaches_the_note_format_in_its_description() {
        // The methodology is the product, so it has to reach the agent through the tool
        // description — an agent that never reads the docs still reads this.
        let add = tool_definitions().into_iter().find(|t| t["name"] == "add_note").unwrap();
        let description = add["description"].as_str().unwrap();
        assert!(description.contains("150"));
        assert!(description.contains("English"));
    }
}
