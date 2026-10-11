//! MCP bridge — exposes MCP tools to the task agent toolset.
//!
//! Only `CapabilityClass::Read` tools are exposed to implementer agents.
//! Write/Exec tools remain gated by the existing capability system.
//!
//! The bridge converts MCP tool definitions into OpenAI-format tool definitions
//! that the model can call, and dispatches execution through the MCP registry.

use crate::capability::{classify, CapabilityClass};
use crate::mcp::McpRegistry;
use serde_json::Value;
use std::path::Path;

/// List MCP tools that are safe for the agent to call (Read class only).
///
/// Returns OpenAI-format tool definitions for tools whose `CapabilityClass`
/// is `Read`. Write and Exec tools are excluded — they require human approval
/// through the capability system and are not exposed to the agent loop.
///
/// A server that is configured but not connected reports no tools, so the
/// returned list is exactly what is reachable right now.
pub async fn list_mcp_tools_for_agent(registry: &McpRegistry) -> Vec<Value> {
    let mut tools = Vec::new();

    // `list_all_schemas` does not exist on `McpRegistry`: schemas are per
    // server (`tool_schemas_for_server`) and the server list carries the
    // connected flag (`list_all_servers`). Enumerate both.
    let servers = match registry.list_all_servers().await {
        Ok(s) => s,
        Err(_) => return tools,
    };

    for server in servers {
        if !server.connected {
            continue;
        }
        for schema in registry.tool_schemas_for_server(&server.name).await {
            let class = classify(&schema.server, &schema.name);
            if class != CapabilityClass::Read {
                continue;
            }

            let mut properties = serde_json::Map::new();
            if let Some(props) = schema.input_schema.get("properties") {
                if let Some(obj) = props.as_object() {
                    for (k, v) in obj {
                        properties.insert(k.clone(), v.clone());
                    }
                }
            }

            let required: Vec<String> = schema
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            tools.push(serde_json::json!({
                "type": "function",
                "function": {
                    "name": mcp_tool_name(&schema.server, &schema.name),
                    "description": schema.description,
                    "parameters": {
                        "type": "object",
                        "properties": properties,
                        "required": required,
                    }
                }
            }));
        }
    }

    tools
}

/// Execute an MCP tool call and return the result as a string.
///
/// Only Read-class tools may be executed. Returns an error for Write/Exec tools.
pub async fn execute_mcp_tool(
    registry: &McpRegistry,
    server: &str,
    tool: &str,
    args: &Value,
    workspace_id: &str,
    _workspace_root: &Path,
) -> anyhow::Result<String> {
    let class = classify(server, tool);
    if class != CapabilityClass::Read {
        return Err(anyhow::anyhow!(
            "MCP tool '{}.{}' is not Read-class (class: {:?}) — requires human approval",
            server,
            tool,
            class
        ));
    }

    // Stamp workspace scope for memory tools
    let args = crate::capability::stamp_workspace_scope(server, tool, args.clone(), workspace_id);

    let outcome = registry
        .call_tool(server, tool, args)
        .await
        .ok_or_else(|| {
            anyhow::anyhow!("MCP tool '{}.{}' not found or not connected", server, tool)
        })?;

    if outcome.success {
        Ok(serde_json::to_string_pretty(&outcome.result).unwrap_or_default())
    } else {
        Err(anyhow::anyhow!(
            "MCP tool '{}.{}' failed: {}",
            server,
            tool,
            outcome.error.unwrap_or_else(|| "unknown error".into())
        ))
    }
}

/// Separator between the server and tool halves of an agent tool name.
///
/// A single `_` is ambiguous: server names and tool names both contain
/// underscores (`sequential_thinking`, `memory_search`), so `mcp_a_b_c` cannot
/// be split unambiguously by "first underscore wins". `::` cannot appear in
/// either half — `validate_server` and the MCP tool names are identifier-like —
/// so it delimits exactly.
const NAME_SEP: &str = "::";

/// Format the agent-facing name for an MCP tool.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp_{}{}{}", server, NAME_SEP, tool)
}

/// Parse an MCP tool name back into (server, tool) components.
///
/// Agent tool names are formatted by [`mcp_tool_name`]. Splitting on the
/// `::` separator (not the first `_`) is what makes server and tool names
/// containing underscores round-trip correctly.
pub fn parse_mcp_tool_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once(NAME_SEP)?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_name_roundtrips_simple_names() {
        let name = mcp_tool_name("rag", "search");
        assert_eq!(name, "mcp_rag::search");
        let (server, tool) = parse_mcp_tool_name(&name).unwrap();
        assert_eq!(server, "rag");
        assert_eq!(tool, "search");
    }

    /// Server and tool names both contain underscores. Splitting on the first
    /// `_` resolved `mcp_sequential_thinking_think` to server `sequential`,
    /// tool `thinking_think` — a call that can never succeed.
    #[test]
    fn mcp_tool_name_roundtrips_underscored_names() {
        let name = mcp_tool_name("sequential_thinking", "think_step");
        let (server, tool) = parse_mcp_tool_name(&name).unwrap();
        assert_eq!(server, "sequential_thinking");
        assert_eq!(tool, "think_step");
    }

    #[test]
    fn mcp_tool_name_roundtrips_memory_tools() {
        let name = mcp_tool_name("memory", "memory_search");
        let (server, tool) = parse_mcp_tool_name(&name).unwrap();
        assert_eq!(server, "memory");
        assert_eq!(tool, "memory_search");
    }

    #[test]
    fn parse_mcp_tool_name_rejects_non_mcp() {
        assert!(parse_mcp_tool_name("write_file").is_none());
    }

    #[test]
    fn parse_mcp_tool_name_rejects_empty() {
        assert!(parse_mcp_tool_name("mcp_").is_none());
        assert!(parse_mcp_tool_name("mcp_rag").is_none());
        assert!(parse_mcp_tool_name("mcp_rag::").is_none());
        assert!(parse_mcp_tool_name("mcp_::search").is_none());
    }

    /// An old-style `mcp_server_tool` name has no `::` and must not be parsed
    /// into a plausible-looking (server, tool) pair — that would dispatch a
    /// call to a server that does not exist.
    #[test]
    fn parse_mcp_tool_name_rejects_underscore_only_form() {
        assert!(parse_mcp_tool_name("mcp_rag_search").is_none());
    }
}
