#!/usr/bin/env bash
set -e

BASE_URL="${GHOSTLINK_URL:-http://127.0.0.1:8003}"

if curl -s "${BASE_URL}/health" >/dev/null 2>&1; then
    echo "Running agentic inference validation suite against ${BASE_URL}..."
    python3 scripts/test_streaming_inference.py
    python3 scripts/test_session_continuity.py
    python3 scripts/test_tool_call_output.py
    python3 scripts/test_token_budget.py
    echo "All agentic inference validation scripts completed successfully!"
else
    echo "Ghostlink server is not running at ${BASE_URL}. Skipping live tests cleanly."
fi
