#!/usr/bin/env python3
import os
import sys
import json
import urllib.request

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

def test_session_continuity():
    session_id = "test-session-continuity-1"
    url = f"{BASE_URL}/api/inference/chat"

    # Turn 1
    p1 = {
        "message": "My favorite color is Cerulean.",
        "session_id": session_id
    }
    req1 = urllib.request.Request(
        url,
        data=json.dumps(p1).encode('utf-8'),
        headers=get_headers()
    )
    with urllib.request.urlopen(req1) as resp:
        body1 = json.loads(resp.read().decode('utf-8'))
        assert body1.get("session_id") == session_id, "session_id mismatch in turn 1"

    # Turn 2
    p2 = {
        "message": "What is my favorite color?",
        "session_id": session_id
    }
    req2 = urllib.request.Request(
        url,
        data=json.dumps(p2).encode('utf-8'),
        headers=get_headers()
    )
    with urllib.request.urlopen(req2) as resp:
        body2 = json.loads(resp.read().decode('utf-8'))
        assert body2.get("session_id") == session_id, "session_id mismatch in turn 2"

    # Check session stats
    stats_url = f"{BASE_URL}/api/sessions/{session_id}/stats"
    req_stats = urllib.request.Request(stats_url, headers=get_headers())
    with urllib.request.urlopen(req_stats) as resp:
        stats = json.loads(resp.read().decode('utf-8'))
        assert stats.get("turn_count", 0) >= 2, f"Expected turn_count >= 2, got {stats.get('turn_count')}"
        assert stats.get("tokens_used", 0) > 0, "Expected tokens_used > 0"
        print("✓ Session continuity & stats verified")

if __name__ == "__main__":
    try:
        test_session_continuity()
        print("Session continuity test passed!")
    except Exception as e:
        print(f"Session continuity test error: {e}")
        sys.exit(1)
