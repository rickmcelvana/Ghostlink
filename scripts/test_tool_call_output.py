#!/usr/bin/env python3
import os
import sys
import json
import urllib.request
import urllib.error

BASE_URL = os.environ.get("GHOSTLINK_URL", "http://127.0.0.1:8003")

API_KEY = os.environ.get("GHOSTLINK_API_KEY")
if not API_KEY:
    for k_path in ["api_key.txt", ".ghostlink/api_key.txt", "crates/ghost-link/api_key.txt"]:
        if os.path.exists(k_path):
            with open(k_path) as kf:
                API_KEY = kf.read().strip()
            break

def get_headers():
    headers = {"Content-Type": "application/json"}
    if API_KEY:
        headers["Authorization"] = f"Bearer {API_KEY}"
    return headers

def test_unsupported_model_tools():
    url = f"{BASE_URL}/v1/chat/completions"
    payload = {
        "model": "unsupported-tools-model",
        "messages": [{"role": "user", "content": "What is the weather?"}],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get current weather",
                    "parameters": {"type": "object", "properties": {"location": {"type": "string"}}}
                }
            }
        ]
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode('utf-8'),
        headers=get_headers()
    )
    try:
        with urllib.request.urlopen(req) as resp:
            print("Expected 400 error, got success!")
            sys.exit(1)
    except urllib.error.HTTPError as e:
        assert e.code == 400, f"Expected 400, got {e.code}"
        err_body = json.loads(e.read().decode('utf-8'))
        code = err_body.get("error", {}).get("code")
        assert code == "unsupported_capability", f"Expected unsupported_capability, got {code}"
        print("✓ Unsupported tool model 4xx error verified")

if __name__ == "__main__":
    try:
        test_unsupported_model_tools()
        print("Tool call test passed!")
    except Exception as e:
        print(f"Tool call test error: {e}")
        sys.exit(1)
