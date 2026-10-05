#!/usr/bin/env python3
"""Contract assertions for core Ghostlink backend API endpoints."""

from __future__ import annotations

import http.client
import json
import os
import ssl
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
HOST = "127.0.0.1"
PORT = 18014
# The server reads `enable_tls` from settings.json, so a developer machine can have it
# on while CI (no settings.json) has it off. Default to plaintext and let the caller
# override, rather than silently failing against a TLS listener with BadStatusLine.
def _detect_scheme() -> str:
    """http unless something says otherwise.

    The server decides from `settings.json`'s `enable_tls`, so a harness that assumes
    plaintext fails with `BadStatusLine` on any developer machine with TLS on -- and a
    harness that assumes TLS fails in CI, which has no settings.json. Read the same file
    the server reads, and let the env var override for a deliberate test.
    """
    override = os.environ.get("GHOSTLINK_TEST_SCHEME")
    if override:
        return override
    try:
        settings = json.loads((ROOT / "settings.json").read_text(encoding="utf-8"))
        return "https" if settings.get("enable_tls") else "http"
    except (OSError, ValueError):
        return "http"


SCHEME = _detect_scheme()
BASE_URL = f"{SCHEME}://{HOST}:{PORT}"


# Same temp path the smoke test writes; never the repository's real api_key.txt.
API_KEY_PATH = Path(tempfile.gettempdir()) / "ghostlink-ci-smoke-key.txt"
# Same value ci_gui_backend_smoke.py uses; both harnesses must agree or
# one will 401 while the other passes.
API_KEY_VALUE = "ghostlink-ci-smoke-key"
# The hashed key store, which is what actually authenticates once it exists.
API_KEYS_STORE_PATH = Path(tempfile.gettempdir()) / "ghostlink-ci-smoke-keys.json"


def _wait_for_api_key(max_wait_s: int = 20) -> str:
    """The server generates and persists a real API key at startup (see
    crates/ghost-link/src/auth.rs) — every route but /health now requires
    it as a bearer token. Polls for the file rather than assuming it's
    already there the instant the process starts."""
    deadline = time.time() + max_wait_s
    while time.time() < deadline:
        try:
            key = API_KEY_PATH.read_text(encoding="utf-8").strip()
            if key:
                return key
        except FileNotFoundError:
            pass
        time.sleep(0.2)
    raise RuntimeError(f"API key file never appeared at {API_KEY_PATH}")


def _auth_headers() -> dict:
    return {"Authorization": f"Bearer {_current_api_key()}"}


# `enable_tls` makes the server serve HTTPS with the local self-signed cert, which
# urlopen rejects by default -- and this is a test harness talking to localhost, so
# verification buys nothing here. Only used when SCHEME is https.
_TLS_CTX = ssl._create_unverified_context() if SCHEME == "https" else None


def _open(req: Request, timeout: float):
    if _TLS_CTX is None:
        return urlopen(req, timeout=timeout)
    return urlopen(req, timeout=timeout, context=_TLS_CTX)


def _current_api_key() -> str:
    """The key the server actually persisted.

    The server generates its own key at startup and overwrites whatever was at the path,
    so the pre-seeded value is only a placeholder that lets the file exist early. Reading
    it back is the only way to learn the real value -- assuming the seed survives is how
    this ends in a 401 that looks like an auth bug.
    """
    deadline = time.time() + 20
    last = ""
    while time.time() < deadline:
        try:
            last = API_KEY_PATH.read_text(encoding="utf-8").strip()
            if last and last != API_KEY_VALUE:
                return last
        except FileNotFoundError:
            pass
        time.sleep(0.2)
    return last or API_KEY_VALUE


def _get(path: str, timeout: float = 5.0, auth: bool = True) -> dict:
    headers = _auth_headers() if auth else {}
    req = Request(f"{BASE_URL}{path}", method="GET", headers=headers)
    with _open(req, timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _post(path: str, payload: dict, timeout: float = 10.0, auth: bool = True) -> dict:
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
    with _open(req, timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _wait_ready(max_wait_s: int = 45) -> None:
    """Waits for the API to answer /health at all.

    Readiness here means "the HTTP server is up". This test starts the backend with no
    model, and `status` is `degraded` in that state rather than the unconditional
    `"healthy"` it used to report -- that literal checked nothing, which is why a chat
    request could fail for 20 seconds while /health claimed all was well.

    The contract is asserted below rather than in the wait: what matters is that the two
    fields agree, not that a model happens to be loaded.
    """
    deadline = time.time() + max_wait_s
    while time.time() < deadline:
        try:
            # 5s, not 1.5s: a direct connection to a closed local port measures ~2,048 ms
            # on this host, reproduced from plain Python with no server involved, so the
            # old budget was unmeetable even when the endpoint answered instantly.
            data = _get("/health", timeout=5.0, auth=False)
            if data.get("status") in ("healthy", "degraded"):
                # A health endpoint that says "healthy" while the backend is unreachable
                # is exactly the defect that was fixed; fail if it ever returns.
                _assert_keys(data, ["backend_reachable"], "/health response")
                if (data.get("status") == "healthy") != bool(data.get("backend_reachable")):
                    raise RuntimeError(
                        "/health status and backend_reachable disagree: "
                        f"{data.get('status')!r} vs {data.get('backend_reachable')!r}"
                    )
                return
        except RuntimeError:
            raise
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
            # `BadStatusLine` and the broader `HTTPException` were missing here, so a
            # truncated or mid-handshake response killed the readiness loop with an
            # unhandled exception instead of being retried.
            time.sleep(0.5)
    raise RuntimeError("backend failed to become healthy")


def _assert_keys(obj: dict, keys: list[str], context: str) -> None:
    missing = [key for key in keys if key not in obj]
    if missing:
        raise AssertionError(f"{context} missing keys: {', '.join(missing)}")


def main() -> int:
    # The server writes its API key to `GHOSTLINK_API_KEY_PATH`, and defaults to the
    # repository's own `api_key.txt` when that is unset. Point it at the same temp file
    # this script polls, and pre-seed it so the very first authenticated request has a
    # key even if startup has not finished persisting.
    #
    # Previously it was left unset, so the server wrote the key to the repo and this
    # script polled a temp path that never appeared -- CI failed with "API key file never
    # appeared". It also means the server had a real, developer-owned key path in play,
    # which is what dd38bea was fixing for the smoke test.
    API_KEY_PATH.parent.mkdir(parents=True, exist_ok=True)
    # Redirect BOTH files, not just the raw key.
    #
    # `load_api_keys()` returns the existing `api_keys.json` store when it parses and is
    # non-empty, and only falls back to seeding from `api_key.txt` otherwise. On a machine
    # that already has a store (any real install) `GHOSTLINK_API_KEY_PATH` alone is simply
    # never read, so the server authenticates against the developer's real keys and this
    # harness cannot possibly succeed. Redirecting the store too makes the run hermetic
    # whether or not a store exists.
    API_KEYS_STORE_PATH.parent.mkdir(parents=True, exist_ok=True)
    for stale in (API_KEY_PATH, API_KEYS_STORE_PATH):
        stale.unlink(missing_ok=True)

    proc = subprocess.Popen(
        ["cargo", "run", "-p", "ghost-link", "--", "serve", HOST, str(PORT)],
        cwd=str(ROOT),
        env={
            **os.environ,
            "GHOSTLINK_API_KEY_PATH": str(API_KEY_PATH),
            "GHOSTLINK_API_KEYS_PATH": str(API_KEYS_STORE_PATH),
        },
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )

    try:
        _wait_ready()

        models = _get("/api/models")
        _assert_keys(
            models,
            ["models", "current_model", "total_models", "loaded_count"],
            "/api/models response",
        )
        if not isinstance(models["models"], list):
            raise AssertionError("/api/models 'models' must be a list")

        model_status = _get("/api/models/status")
        _assert_keys(
            model_status,
            ["loaded_models", "downloading_models", "current_model"],
            "/api/models/status response",
        )
        if not isinstance(model_status["loaded_models"], list):
            raise AssertionError("/api/models/status 'loaded_models' must be a list")

        runtime_recommend = _get("/api/runtime/recommend")
        _assert_keys(
            runtime_recommend,
            ["detected_runtime", "available_memory_gb", "recommended_models", "count"],
            "/api/runtime/recommend response",
        )
        if not isinstance(runtime_recommend["recommended_models"], list):
            raise AssertionError("/api/runtime/recommend 'recommended_models' must be a list")

        ollama_health = _get("/api/ollama/health")
        _assert_keys(
            ollama_health,
            ["reachable", "ollama_url", "model_count", "detail"],
            "/api/ollama/health response",
        )

        chat = _post(
            "/api/inference/chat",
            {
                "message": "contract-test",
                "model": "neural-chat",
                "max_tokens": 32,
                "ollama_url": "http://127.0.0.1:11434",
            },
            timeout=20.0,
        )
        _assert_keys(
            chat,
            [
                "response",
                "request_id",
                "session_id",
                "model",
                "ollama_url",
                "tokens_estimated",
                "exec_tokens",
                "exec_micro_batch",
            ],
            "/api/inference/chat response",
        )

        if not isinstance(chat["response"], str) or not chat["response"]:
            raise AssertionError("/api/inference/chat response must be non-empty string")

        session_id = str(chat["session_id"])
        cancel_result = _post(f"/api/sessions/{session_id}/cancel", {}, timeout=10.0)
        _assert_keys(cancel_result, ["status", "session_id", "cancelled"], "session cancel response")

        print("API contract tests passed")
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
        print(f"API contract tests failed: {exc}", file=sys.stderr)
        raise
