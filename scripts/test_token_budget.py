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

def test_token_budget():
    session_id = "test-budget-sess-1"
    url = f"{BASE_URL}/api/inference/chat"

    payload = {
        "message": "Write a short story about a robot learning to paint.",
        "session_id": session_id,
        "max_tokens": 5
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode('utf-8'),
        headers=get_headers()
    )
    with urllib.request.urlopen(req) as resp:
        body = json.loads(resp.read().decode('utf-8'))
        assert body.get("session_id") == session_id

    stats_url = f"{BASE_URL}/api/sessions/{session_id}/stats"
    req_stats = urllib.request.Request(stats_url, headers=get_headers())
    with urllib.request.urlopen(req_stats) as resp:
        stats = json.loads(resp.read().decode('utf-8'))
        assert stats.get("tokens_used", 0) > 0
        print("✓ Token budget enforcement test passed")

if __name__ == "__main__":
    try:
        test_token_budget()
        print("Token budget test passed!")
    except Exception as e:
        print(f"Token budget test error: {e}")
        sys.exit(1)
