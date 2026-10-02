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

def test_v1_chat_streaming():
    url = f"{BASE_URL}/v1/chat/completions"
    payload = {
        "model": "ghostlink-default",
        "messages": [{"role": "user", "content": "Count from 1 to 3."}],
        "stream": True
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode('utf-8'),
        headers=get_headers()
    )
    with urllib.request.urlopen(req) as resp:
        content_type = resp.headers.get("Content-Type", "")
        assert "text/event-stream" in content_type, f"Expected event-stream, got {content_type}"
        lines = resp.read().decode('utf-8').splitlines()
        data_lines = [l for l in lines if l.startswith("data: ")]
        assert len(data_lines) > 0, "No data events received"
        last_event = json.loads(data_lines[-1][6:])
        assert last_event.get("done") is True, "Last streaming event should have done=True"
        print("✓ /v1/chat/completions streaming passed")

def test_v1_completions_streaming():
    url = f"{BASE_URL}/v1/completions"
    payload = {
        "model": "ghostlink-default",
        "prompt": "The quick brown fox",
        "stream": True
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode('utf-8'),
        headers=get_headers()
    )
    with urllib.request.urlopen(req) as resp:
        content_type = resp.headers.get("Content-Type", "")
        assert "text/event-stream" in content_type, f"Expected event-stream, got {content_type}"
        lines = resp.read().decode('utf-8').splitlines()
        data_lines = [l for l in lines if l.startswith("data: ")]
        assert len(data_lines) > 0, "No data events received"
        last_event = json.loads(data_lines[-1][6:])
        assert last_event.get("done") is True, "Last streaming event should have done=True"
        print("✓ /v1/completions streaming passed")

def test_api_inference_chat_streaming():
    url = f"{BASE_URL}/api/inference/chat"
    payload = {
        "message": "Hello world",
        "stream": True
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode('utf-8'),
        headers=get_headers()
    )
    with urllib.request.urlopen(req) as resp:
        content_type = resp.headers.get("Content-Type", "")
        assert "text/event-stream" in content_type, f"Expected event-stream, got {content_type}"
        lines = resp.read().decode('utf-8').splitlines()
        data_lines = [l for l in lines if l.startswith("data: ")]
        assert len(data_lines) > 0, "No data events received"
        print("✓ /api/inference/chat streaming passed")

if __name__ == "__main__":
    try:
        test_v1_chat_streaming()
        test_v1_completions_streaming()
        test_api_inference_chat_streaming()
        print("All streaming inference tests passed!")
    except Exception as e:
        print(f"Streaming test error: {e}")
        sys.exit(1)
