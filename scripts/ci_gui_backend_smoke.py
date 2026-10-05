#!/usr/bin/env python3
"""CI smoke test for Ghostlink GUI backend API surfaces."""

from __future__ import annotations

import http.client
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
HOST = "127.0.0.1"
PORT = 18013
BASE_URL = f"http://{HOST}:{PORT}"
BINARY_PATH = Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target") / "debug" / (
    "ghost-link.exe" if sys.platform == "win32" else "ghost-link"
)


API_KEY_PATH = ROOT / "api_key.txt"
API_KEY_VALUE = "ghostlink-ci-smoke-key"


def _auth_headers() -> dict:
    return {"Authorization": f"Bearer {API_KEY_VALUE}"}


def _get_json(path: str, timeout: float = 5.0, auth: bool = True) -> dict:
    headers = _auth_headers() if auth else {}
    req = Request(f"{BASE_URL}{path}", method="GET", headers=headers)
    with urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _post_json(path: str, payload: dict, timeout: float = 10.0, auth: bool = True) -> dict:
    body = json.dumps(payload).encode("utf-8")
    headers = {"Content-Type": "application/json"}
    if auth:
        headers.update(_auth_headers())
    req = Request(
        f"{BASE_URL}{path}",
        data=body,
        headers=headers,
        method="POST",
    )
    with urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _wait_for_health(max_wait_s: int = 45) -> None:
    """Waits for the API to answer /health at all.

    Readiness here means "the HTTP server is up", not "a model is loaded". CI starts the
    backend with no model, and `status` is now `degraded` in that state rather than the
    unconditional `"healthy"` it used to report -- that literal checked nothing, which is
    why a chat request could fail for 20 seconds while `/health` claimed all was well.
    So the loop waits for a parsable body and nothing more, and the caller below asserts
    the specific fields it cares about.
    """
    deadline = time.time() + max_wait_s
    while time.time() < deadline:
        try:
            # 5s, not the previous 1.5s. A *direct* connection to a closed local port
            # measures ~2,048 ms on this host -- reproduced from plain Python with no
            # server involved -- so the old budget could not be met even when the
            # endpoint answered instantly. Raising it removes a flake that had nothing
            # to do with the code under test.
            _get_json("/health", timeout=5.0, auth=False)
            return
        except (
            URLError,
            HTTPError,
            TimeoutError,
            OSError,
            ValueError,
            http.client.BadStatusLine,
            http.client.HTTPException,
            ConnectionError,
        ):
            # `BadStatusLine` and friends were missing here, so a response that arrived
            # truncated or mid-handshake escaped the readiness loop as an unhandled
            # exception instead of being retried. That is how this job failed in CI.
            time.sleep(0.5)
    raise RuntimeError("backend health endpoint did not become ready")


def _build_backend() -> None:
    subprocess.run(
        ["cargo", "build", "-p", "ghost-link", "--bin", "ghost-link"],
        cwd=str(ROOT),
        check=True,
        stdout=None,
        stderr=None,
    )


def main() -> int:
    _build_backend()
    API_KEY_PATH.write_text(API_KEY_VALUE, encoding="utf-8")

    proc = subprocess.Popen(
        [str(BINARY_PATH), "serve", HOST, str(PORT)],
        cwd=str(ROOT),
        env={**os.environ, "GHOSTLINK_API_KEY_PATH": str(API_KEY_PATH)},
        stdout=None,
        stderr=None,
        preexec_fn=None,
    )

    try:
        _wait_for_health()

        health = _get_json("/health", auth=False)
        # `status` is `degraded` whenever no model is loaded, which is the expected state
        # in CI. What must hold is that the endpoint reports the truth about it, so the
        # contract asserted here is the *shape* and the consistency between the two
        # fields, not a value that used to be a hardcoded literal.
        if health.get("status") not in ("healthy", "degraded"):
            raise RuntimeError(
                f"health endpoint returned an unknown status: {health.get('status')!r}"
            )
        if "backend_reachable" not in health:
            raise RuntimeError("health endpoint missing backend_reachable field")
        if (health.get("status") == "healthy") != bool(health.get("backend_reachable")):
            raise RuntimeError(
                "health status and backend_reachable disagree: "
                f"{health.get('status')!r} vs {health.get('backend_reachable')!r}"
            )

        models = _get_json("/api/models")
        if not isinstance(models.get("models"), list):
            raise RuntimeError("/api/models did not return a models list")

        ollama_health = _get_json("/api/ollama/health")
        if "reachable" not in ollama_health:
            raise RuntimeError("/api/ollama/health missing reachable field")

        chat = _post_json(
            "/api/inference/chat",
            {
                "message": "ci smoke",
                "model": "neural-chat",
                "max_tokens": 32,
                "ollama_url": "http://127.0.0.1:11434",
            },
            timeout=20.0,
        )
        response = str(chat.get("response", ""))
        if not response:
            raise RuntimeError("chat response was empty")

        print("GUI backend smoke passed")
        return 0
    finally:
        if proc.poll() is None:
            proc.send_signal(signal.SIGTERM)
            try:
                proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"GUI backend smoke failed: {exc}", file=sys.stderr)
        raise
