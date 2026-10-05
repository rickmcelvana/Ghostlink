//! Model-driven tool-calling: schema injection into the prompt, ReAct-style parsing
//! of the model's tool-call intent, and the observation text fed back on the next
//! turn. Works with any local model (no reliance on a model-specific tool-calling
//! chat template) since Ghostlink runs arbitrary GGUF/Ollama models.

use serde_json::Value;

use super::client::McpToolSchema;

/// Hard cap on tool round-trips per user turn, so a model that keeps requesting
/// tools (or misunderstands the marker) can't loop forever.
///
/// Raised from 3 to 6 (2026-08-10): 3 was cutting off legitimate multi-step
/// agent tasks (e.g. filesystem lookup -> compute -> write back) before they
/// could finish, forcing the "(stopped after N tool round-trips without a
/// final answer)" bailout even on the happy path. Each round trip is one
/// real inference call, so this doubles the worst-case latency/compute for a
/// turn where the model is genuinely stuck — that tradeoff is deliberate,
/// not an oversight. Context-window headroom for the accumulated scratchpad
/// across more rounds is the model-load `ctx_size` setting's job (see the
/// Model Performance section in the GUI, or `GHOSTLINK_CTX_SIZE`), not this
/// constant's — raising one without enough of the other just moves the
/// failure mode from "gave up early" to "truncated mid-loop."
pub const MAX_TOOL_ITERATIONS: usize = 6;

const MARKER: &str = "TOOL_CALL:";

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedToolCall {
    pub tool: String,
    pub args: Value,
}

/// A compact call signature derived from a JSON schema: `(required: a, b; optional: c)`.
///
/// Falls back to an empty string for anything it does not recognise, so an unusual
/// schema degrades to "no signature" rather than to a wrong one -- the model still has
/// the tool name and description and can attempt a call.
fn signature_of(schema: &serde_json::Value) -> String {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return String::new();
    };
    if props.is_empty() {
        return "()".to_string();
    }
    let required: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let req_names: Vec<&str> = props
        .keys()
        .map(|s| s.as_str())
        .filter(|k| required.iter().any(|r| r == k))
        .collect();
    let opt_names: Vec<&str> = props
        .keys()
        .map(|s| s.as_str())
        .filter(|k| !required.iter().any(|r| r == k))
        .collect();
    let mut out = String::from("(");
    if !req_names.is_empty() {
        out.push_str("required: ");
        out.push_str(&req_names.join(", "));
    }
    if !opt_names.is_empty() {
        if !out.is_empty() && out != "(" {
            out.push_str("; ");
        }
        out.push_str("optional: ");
        out.push_str(&opt_names.join(", "));
    }
    out.push(')');
    out
}

/// The first sentence of a tool description.
///
/// MCP tool descriptions routinely run to several sentences, often with examples. The
/// first sentence is what the tool *is*; the rest is usage detail that belongs in the
/// full schema rather than in every prompt.
fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.find(['.', '!', '?']) {
        Some(i) if i > 0 => trimmed[..=i].trim().to_string(),
        _ => trimmed.to_string(),
    }
}

/// The full JSON schema for one tool, for on-demand detail.
///
/// Included in the prompt only when a tool's own signature is ambiguous, which the
/// model cannot currently signal. Kept as a function so the detail has one place to come
/// from when that path is built, rather than being re-derived inline.
pub fn tool_schema_detail(tools: &[McpToolSchema], name: &str) -> Option<String> {
    tools.iter().find(|t| t.name == name).map(|t| {
        format!(
            "{} — {}\ninput schema: {}",
            t.name, t.description, t.input_schema
        )
    })
}

/// Builds the instructions block describing every enabled tool, meant to be
/// prefixed onto the system/user prompt actually sent to the model. Returns an
/// empty string when there are no tools to offer (so callers can skip injection
/// entirely rather than sending a pointless empty header).
pub fn build_tool_instructions(tools: &[McpToolSchema]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let mut block = String::from(
        "You have access to tools. To call one, reply with ONLY a single line of the \
         EXACT form below (not a function-call-looking expression, not any other syntax):\n\
         TOOL_CALL: {\"tool\": \"<tool_name>\", \"args\": { ... }}\n\
         Nothing else on that line — no other text before or after it.\n\n\
         Example:\n\
         User: What is 9 times 6?\n\
         Assistant: TOOL_CALL: {\"tool\": \"calculate\", \"args\": {\"expression\": \"9 * 6\"}}\n\
         (the tool result is then given to you as \"Observation: ...\")\n\
         Assistant: 9 times 6 is 54.\n\n\
         Rules:\n\
         - As soon as you receive an Observation, write your final answer in plain text \
         using that result. Do not call the same tool again for the same question.\n\
         - If no tool is needed, just answer normally in plain text.\n\n\
         Available tools:\n",
    );

    // Signatures, not full JSON schemas.
    //
    // This block is prefixed to *every* prompt (`main.rs`: `format!("{tool_instructions}
    // Question: {user_message}")`), so its size is a fixed per-request prefill cost.
    // Measured by proxying the real request bodies: a five-word question produced a
    // 17,149-character user message and 3,814 prompt tokens, of which ~3,700 were the
    // tool catalog. At the measured 292 tok/s that is ~13 seconds of prefill before
    // the model reads the question.
    //
    // A signature line carries what the model needs to decide *whether* to call a tool
    // and *what* arguments it takes. The full schema is available on demand via
    // `tool_schema_detail`, and a call that guesses an argument name comes back as a
    // validation error rather than a silent wrong call.
    for tool in tools {
        let description = if tool.description.is_empty() {
            "(no description)"
        } else {
            tool.description.as_str()
        };
        let signature = signature_of(&tool.input_schema);
        if signature.is_empty() {
            // No signature means the schema was not a shape we recognise, so the compact
            // form would leave the model guessing. Pay the full cost for this one tool
            // rather than for all of them.
            block.push_str(
                &tool_schema_detail(std::slice::from_ref(tool), &tool.name)
                    .unwrap_or_else(|| format!("- {} — {}", tool.name, description)),
            );
        } else {
            block.push_str(&format!(
                "- {}{} — {}\n",
                tool.name,
                signature,
                first_sentence(description),
            ));
        }
    }

    block
}

/// Parses a `TOOL_CALL: {...}` marker out of the model's generated text. Tracks
/// brace depth (rather than trying to `serde_json::from_str` the whole remainder)
/// so trailing commentary after the JSON object doesn't break parsing.
pub fn extract_tool_call(text: &str) -> Option<ParsedToolCall> {
    let marker_pos = text.find(MARKER)?;
    let after_marker = &text[marker_pos + MARKER.len()..];
    let json_start_rel = after_marker.find('{')?;
    let json_region = &after_marker[json_start_rel..];

    let mut depth = 0i32;
    let mut end = None;
    for (i, ch) in json_region.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + ch.len_utf8());
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let json_str = &json_region[..end];

    let parsed: Value = serde_json::from_str(json_str).ok()?;
    let tool = parsed.get("tool")?.as_str()?.to_string();
    let args = parsed.get("args").cloned().unwrap_or(Value::Null);
    Some(ParsedToolCall { tool, args })
}

/// Hard cap (in characters) on a single tool result folded back into the prompt.
/// A `fetch` call pulling a whole webpage (nav, footer, ads, unrelated content) can
/// otherwise blow the entire context budget in one shot regardless of how large
/// `--ctx-size` is configured — this bounds the damage a single observation can do.
const MAX_OBSERVATION_CHARS: usize = 4000;

/// Truncates `text` to `MAX_OBSERVATION_CHARS`, appending a marker noting how much
/// was cut so the model (and anyone reading the transcript) knows content is missing
/// rather than silently seeing a shortened result as if it were complete.
fn truncate_observation(text: &str) -> std::borrow::Cow<'_, str> {
    let total_chars = text.chars().count();
    if total_chars <= MAX_OBSERVATION_CHARS {
        return std::borrow::Cow::Borrowed(text);
    }
    let kept: String = text.chars().take(MAX_OBSERVATION_CHARS).collect();
    let omitted = total_chars - MAX_OBSERVATION_CHARS;
    std::borrow::Cow::Owned(format!(
        "{kept}... [truncated, {omitted} more characters omitted]"
    ))
}

/// Formats a tool result as an "Observation" turn appended to the running prompt
/// before asking the model to continue.
pub fn format_observation(tool: &str, result_json: &Value) -> String {
    let rendered = result_json.to_string();
    let observation = truncate_observation(&rendered);
    format!("\nObservation ({tool}): {observation}\n")
}

/// Formats a denial (confirmation gate rejected by the user) as an observation.
pub fn format_denial(tool: &str) -> String {
    format!("\nObservation ({tool}): the user denied permission to run this tool.\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_marker_returns_none() {
        assert_eq!(extract_tool_call("just a normal answer"), None);
    }

    #[test]
    fn parses_simple_call() {
        let text = r#"TOOL_CALL: {"tool": "calculate", "args": {"expression": "2+2"}}"#;
        let call = extract_tool_call(text).unwrap();
        assert_eq!(call.tool, "calculate");
        assert_eq!(call.args, serde_json::json!({"expression": "2+2"}));
    }

    #[test]
    fn tolerates_leading_and_trailing_commentary() {
        let text = "Sure, let me check that.\nTOOL_CALL: {\"tool\": \"read_text_file\", \"args\": {\"path\": \"a.txt\"}}\nI'll wait for the result.";
        let call = extract_tool_call(text).unwrap();
        assert_eq!(call.tool, "read_text_file");
        assert_eq!(call.args, serde_json::json!({"path": "a.txt"}));
    }

    #[test]
    fn handles_nested_braces_in_args() {
        let text = r#"TOOL_CALL: {"tool": "api_call", "args": {"body": {"nested": {"a": 1}}}}"#;
        let call = extract_tool_call(text).unwrap();
        assert_eq!(call.tool, "api_call");
        assert_eq!(call.args, serde_json::json!({"body": {"nested": {"a": 1}}}));
    }

    #[test]
    fn missing_tool_field_returns_none() {
        let text = r#"TOOL_CALL: {"args": {}}"#;
        assert_eq!(extract_tool_call(text), None);
    }

    #[test]
    fn no_args_defaults_to_null() {
        let text = r#"TOOL_CALL: {"tool": "list_allowed_directories"}"#;
        let call = extract_tool_call(text).unwrap();
        assert_eq!(call.tool, "list_allowed_directories");
        assert_eq!(call.args, Value::Null);
    }

    #[test]
    fn instructions_block_is_empty_for_no_tools() {
        assert_eq!(build_tool_instructions(&[]), "");
    }

    #[test]
    fn format_observation_passes_through_small_results_untouched() {
        let result = serde_json::json!({"content": "hello world"});
        let observation = format_observation("fetch", &result);
        assert_eq!(
            observation,
            "\nObservation (fetch): {\"content\":\"hello world\"}\n"
        );
    }

    #[test]
    fn format_observation_truncates_oversized_results() {
        let huge_text = "x".repeat(10_000);
        let result = serde_json::json!({"content": huge_text});
        let observation = format_observation("fetch", &result);
        assert!(
            observation.len() < 5_000,
            "expected truncation, got {} chars",
            observation.len()
        );
        assert!(observation.contains("truncated"));
        assert!(observation.contains("more characters omitted"));
    }

    #[test]
    fn format_observation_truncation_is_utf8_safe() {
        // Multi-byte chars near the truncation boundary shouldn't panic or split
        // a character in half.
        let huge_text = "é".repeat(5_000);
        let result = serde_json::json!({"content": huge_text});
        let observation = format_observation("fetch", &result);
        assert!(observation.contains("truncated"));
    }

    #[test]
    fn instructions_block_lists_each_tool() {
        let tools = vec![McpToolSchema {
            server: "calculator".to_string(),
            name: "calculate".to_string(),
            description: "Evaluate a math expression".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let block = build_tool_instructions(&tools);
        assert!(block.contains("TOOL_CALL:"));
        assert!(block.contains("calculate"));
        assert!(block.contains("Evaluate a math expression"));
    }

    #[test]
    fn a_signature_names_required_and_optional_arguments() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file"},
                "content": {"type": "string", "description": "Content to write"},
                "encoding": {"type": "string", "description": "Text encoding"}
            },
            "required": ["path"]
        });
        assert_eq!(
            signature_of(&schema),
            "(required: path; optional: content, encoding)"
        );
    }

    #[test]
    fn a_signature_with_no_properties_is_empty_parens() {
        let schema = serde_json::json!({"type": "object", "properties": {}});
        assert_eq!(signature_of(&schema), "()");
    }

    #[test]
    fn an_unrecognised_schema_yields_no_signature_rather_than_a_wrong_one() {
        // Better no signature than a misleading one: the model still has the name and
        // description and can attempt a call.
        assert_eq!(signature_of(&serde_json::json!({"type": "string"})), "");
        assert_eq!(signature_of(&serde_json::json!({})), "");
    }

    #[test]
    fn a_signature_is_ordered_and_deterministic() {
        // serde_json maps preserve insertion order by default, but the block is a
        // prompt prefix, so nondeterminism here would be a real regression.
        let schema = serde_json::json!({
            "properties": {"a": {}, "b": {}, "c": {}},
            "required": ["b"]
        });
        let first = signature_of(&schema);
        assert_eq!(first, signature_of(&schema));
        assert_eq!(first, "(required: b; optional: a, c)");
    }

    #[test]
    fn the_signature_is_far_smaller_than_the_schema_it_replaces() {
        // The whole point. A realistic nested schema with per-property descriptions is
        // what cost ~3,700 tokens across 14 tools.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "The absolute path to the file or directory to operate on. Must be inside the workspace root."},
                "content": {"type": "string", "description": "The content to write. Interpreted as UTF-8 unless an encoding is given."},
                "encoding": {"type": "string", "enum": ["utf8", "base64"], "description": "The text encoding to use when reading or writing."}
            },
            "required": ["path"],
            "additionalProperties": false
        });
        let sig = signature_of(&schema);
        let full = schema.to_string();
        assert!(
            sig.len() * 5 < full.len(),
            "signature ({}) should be far smaller than the schema ({})",
            sig.len(),
            full.len()
        );
    }

    #[test]
    fn first_sentence_keeps_only_the_sentence() {
        assert_eq!(
            first_sentence("Read a file. Use this when you need contents."),
            "Read a file."
        );
        assert_eq!(first_sentence("  Read a file.  "), "Read a file.");
        assert_eq!(first_sentence("No trailing period"), "No trailing period");
        assert_eq!(
            first_sentence("Ends with a question? Yes."),
            "Ends with a question?"
        );
        assert_eq!(first_sentence(""), "");
    }

    #[test]
    fn the_tool_block_omits_the_full_schema() {
        // The regression guard: `build_tool_instructions` used to interpolate
        // `tool.input_schema` verbatim, which is what put ~3,700 tokens in every prompt.
        let tools = vec![McpToolSchema {
            server: "filesystem".to_string(),
            name: "read_file".to_string(),
            description: "Read a file from disk. Returns its contents as text.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path, must be inside the workspace root."},
                    "encoding": {"type": "string", "description": "Text encoding."}
                },
                "required": ["path"]
            }),
        }];
        let block = build_tool_instructions(&tools);
        assert!(block.contains("read_file"), "tool missing: {block}");
        assert!(
            block.contains("required: path"),
            "signature missing: {block}"
        );
        assert!(
            !block.contains("input schema:"),
            "a tool with a usable signature must not carry its full schema: {block}"
        );
        assert!(
            !block.contains("workspace root"),
            "the per-property description leaked in: {block}"
        );
    }

    #[test]
    fn the_full_schema_is_still_reachable_on_demand() {
        // Compacting the prompt must not lose the detail -- it moves it behind a lookup.
        let tools = vec![McpToolSchema {
            server: "filesystem".to_string(),
            name: "read_file".to_string(),
            description: "Read a file.".to_string(),
            input_schema: serde_json::json!({"properties": {"path": {"description": "inside workspace"}}}),
        }];
        let detail = tool_schema_detail(&tools, "read_file").expect("tool exists");
        assert!(
            detail.contains("workspace"),
            "detail lost the schema: {detail}"
        );
        assert!(tool_schema_detail(&tools, "nope").is_none());
    }

    #[test]
    fn an_unrecognised_schema_falls_back_to_the_full_schema_for_that_one_tool() {
        // Compact form must not become "no information". A tool whose schema we cannot
        // summarise gets the full schema, paid for one tool rather than all of them.
        let tools = vec![McpToolSchema {
            server: "x".to_string(),
            name: "odd_tool".to_string(),
            description: "Does something unusual.".to_string(),
            input_schema: serde_json::json!({"oneOf": [{"a": 1}, {"b": 2}]}),
        }];
        let block = build_tool_instructions(&tools);
        assert!(block.contains("input schema:"), "detail missing: {block}");
        assert!(block.contains("oneOf"), "schema not inlined: {block}");
    }
}
