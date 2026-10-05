# Prompt overhead: measured, not inferred

Captured by proxying Ghostlink's own requests to `llama-server` (a local recording
proxy on 8099 forwarding to 8080) so the bodies are visible *and* timings stay real.

## The capture

Request: `{"message":"Reply with exactly: OK","max_tokens":4}`

    POST /v1/chat/completions
      system   313 chars
      user    17149 chars      <-- for a five-word question

    POST /completion
      prompt  17482 chars

`prompt_tokens` reported by llama-server for that turn: **3814**.

A five-word question costs 3,814 prompt tokens.

## Where they go

`main.rs:3188` (and the same shape in `native_tool_loop_core`):

```rust
let prompt = format!("{tool_instructions}Question: {user_message}\n{scratchpad}");
```

`tool_instructions` is `grounding::capability_statement(...)` plus
`mcp::toolcall::build_tool_instructions(tools)`, which inlines every enabled tool's
**full JSON input schema as text**:

```rust
block.push_str(&format!(
    "- {} - {}\n  input schema: {}\n",
    tool.name, description, tool.input_schema
));
```

With the `filesystem` MCP server enabled that is 14 tools. The measured delta between
a tool-enabled turn and a `tools: []` turn was **zero**, which is the trap here: the
block is built from the *slot*'s schemas before the per-request tool list is applied,
so toggling `mcp.tools` does not remove it. Every request pays it.

## Cost

At the measured 292 tok/s prefill on this host:

    3814 tokens / 292 tok/s = 13.1 seconds of prefill

for a five-word question, before the model has read the question.

## What was verified

- The 3,814-token figure is from llama-server's own `usage.prompt_tokens`.
- The 17,149-character user message is a captured request body, not an estimate.
- Direct `/v1/chat/completions` with the same system prompt and a short message costs
  **48** tokens. The 3,766-token gap is entirely Ghostlink's prompt assembly.
- Recall is not the cause: a turn with history (recall disabled) measured 3,822.
- Tool *selection* is not the cause: `tools: []`, `tools: ["file_operations"]` and an
  absent `tools` field all measured 3,814.

## Why this was not found earlier

Every measurement in this branch used a long synthetic prompt where 3,800 tokens of
fixed overhead is noise against 5,000 tokens of user text. The overhead only becomes
visible when the user text is short -- which is the common case in a GUI chat.
