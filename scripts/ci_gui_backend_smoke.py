#!/usr/bin/env python3
"""CI smoke test for Ghostlink GUI backend API surfaces."""

from __future__ import annotations

import http.client
import json
import os
import signal
import subprocess
import sys
import ssl
import tempfile
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
HOST = "127.0.0.1"
PORT = 18013
# The server honours `enable_tls` from settings.json, so a developer machine can serve
# HTTPS while CI (no settings.json) serves plaintext. Default to plaintext, allow an
# override, and skip cert verification when https -- this only ever talks to localhost.
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
_TLS_CTX = ssl._create_unverified_context() if SCHEME == "https" else None
BASE_URL = f"{SCHEME}://{HOST}:{PORT}"


def _open(req, timeout):
    if _TLS_CTX is None:
        return urlopen(req, timeout=timeout)
    return urlopen(req, timeout=timeout, context=_TLS_CTX)
BINARY_PATH = Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target") / "debug" / (
    "ghost-link.exe" if sys.platform == "win32" else "ghost-link"
)


# The throwaway key file lives in TEMP, never in the repository.
#
# This used to be `ROOT / "api_key.txt"` with an unconditional overwrite, which destroyed
# the developer's real bootstrap key on every run: `api_keys.json` keeps a SHA-256 of
# the original, so after a smoke run the key on disk no longer authenticated and there
# was no way back -- the plaintext is not recoverable from a one-way hash. The `finally`
# block killed the server but never restored the file.
#
# A test must not be able to destroy the thing it is testing.
API_KEY_PATH = Path(tempfile.gettempdir()) / "ghostlink-ci-smoke-key.txt"
# The hashed store that actually authenticates once it exists.
API_KEYS_STORE_PATH = Path(tempfile.gettempdir()) / "ghostlink-ci-smoke-keys.json"
API_KEY_VALUE = "ghostlink-ci-smoke-key"


def _server_api_key(max_wait_s: float = 20.0) -> str:
    """The key the server actually persisted, waiting for it to change.

    The server generates its own key at startup and overwrites the seed we wrote, so a
    read that happens too early returns the seed -- which then 401s and looks like an
    auth bug rather than a race. Waiting for the value to *change* is what makes this
    deterministic.
    """
    deadline = time.monotonic() + max_wait_s
    value = ""
    while time.monotonic() < deadline:
        try:
            value = API_KEY_PATH.read_text(encoding="utf-8").strip()
        except OSError:
            value = ""
        if value and value != API_KEY_VALUE:
            return value
        time.sleep(0.2)
    return value or API_KEY_VALUE


def _auth_headers() -> dict:
    # The server generates its own key at startup and overwrites whatever was at the
    # path, so the literal below is only correct while the file is still the seed we
    # wrote. Reading it back is the only way to learn the value the server will accept.
    return {"Authorization": f"Bearer {_server_api_key()}"}


def _get_json(path: str, timeout: float = 5.0, auth: bool = True) -> dict:
    headers = _auth_headers() if auth else {}
    req = Request(f"{BASE_URL}{path}", method="GET", headers=headers)
    with _open(req, timeout) as resp:
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
    with _open(req, timeout) as resp:
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
    # Clear BOTH before starting. A store left behind by an earlier run (or by the other
    # harness, which uses the same filenames) is authoritative -- `load_api_keys()`
    # returns it and never consults the raw key -- so a stale store makes this run
    # authenticate against keys nobody in this process knows.
    API_KEY_PATH.unlink(missing_ok=True)
    API_KEYS_STORE_PATH.unlink(missing_ok=True)
    API_KEY_PATH.write_text(API_KEY_VALUE, encoding="utf-8")

    proc = subprocess.Popen(
        [str(BINARY_PATH), "serve", HOST, str(PORT)],
        cwd=str(ROOT),
        env={
            **os.environ,
            # Both, not just the raw key: `load_api_keys()` returns an existing
            # `api_keys.json` store when it parses, and only falls back to seeding from
            # `api_key.txt` otherwise. Redirecting only the raw key means a machine with
            # a real store authenticates against real keys and this harness cannot pass.
            "GHOSTLINK_API_KEY_PATH": str(API_KEY_PATH),
            "GHOSTLINK_API_KEYS_PATH": str(API_KEYS_STORE_PATH),
        },
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
        # Remove the throwaway key so it cannot be mistaken for a real one later.
        try:
            API_KEY_PATH.unlink()
        except OSError:
            pass


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"GUI backend smoke failed: {exc}", file=sys.stderr)
        raise
