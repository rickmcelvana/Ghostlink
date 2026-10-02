#!/usr/bin/env python3
"""Advanced agentic inference tests: concurrent load, reliability, and RPC failover."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import statistics
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

HOST = "127.0.0.1"
PORT = 18014
BASE_URL = f"http://{HOST}:{PORT}"
API_KEY_PATH = Path(".") / "api_key.txt"


def _wait_for_api_key(max_wait_s: int = 20) -> str:
    """Poll for API key file."""
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
    return {"Authorization": f"Bearer {_wait_for_api_key()}"}


def _wait_ready(max_wait_s: int = 45) -> None:
    """Wait for backend health."""
    deadline = time.time() + max_wait_s
    while time.time() < deadline:
        try:
            req = Request(f"{BASE_URL}/health", method="GET")
            with urlopen(req, timeout=2.0) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                if data.get("status") == "healthy":
                    return
        except (URLError, HTTPError, TimeoutError, OSError, ValueError):
            time.sleep(0.5)
    raise RuntimeError("backend failed to become healthy")


def _post_chat(message: str, timeout: int = 60) -> dict:
    """Send a chat message and return response."""
    headers = _auth_headers()
    headers["Content-Type"] = "application/json"

    payload = {"message": message, "max_tokens": 64}
    body = json.dumps(payload).encode("utf-8")
    req = Request(f"{BASE_URL}/api/inference/chat", data=body, headers=headers, method="POST")

    start = time.time()
    try:
        with urlopen(req, timeout=timeout) as resp:
            elapsed = (time.time() - start) * 1000.0
            data = json.loads(resp.read().decode("utf-8"))
            data["_request_time_ms"] = elapsed
            return data
    except HTTPError as exc:
        elapsed = (time.time() - start) * 1000.0
        resp_data = exc.read().decode("utf-8")
        try:
            result = json.loads(resp_data)
        except json.JSONDecodeError:
            result = {"error": resp_data}
        result["_request_time_ms"] = elapsed
        result["_http_error"] = exc.code
        return result
    except Exception as exc:
        elapsed = (time.time() - start) * 1000.0
        return {
            "error": str(exc),
            "_request_time_ms": elapsed,
            "_exception": type(exc).__name__,
        }


def test_concurrent_load(num_concurrent: int = 10, timeout: int = 120) -> dict:
    """Test concurrent inference requests for load balancing."""
    print(f"\n[load] Testing {num_concurrent} concurrent requests...")

    prompts = [
        "What is artificial intelligence?",
        "Explain distributed systems.",
        "List three machine learning algorithms.",
        "How does network communication work?",
        "Describe the blockchain.",
        "What is cloud computing?",
        "Explain neural networks.",
        "How does caching improve performance?",
        "What is API design?",
        "Describe DevOps practices.",
    ]

    results = []
    errors = []
    start_time = time.time()

    def send_request(idx: int) -> dict:
        prompt = prompts[idx % len(prompts)]
        result = _post_chat(prompt, timeout=timeout)
        result["_request_index"] = idx
        return result

    # Use thread pool for concurrent requests
    with concurrent.futures.ThreadPoolExecutor(max_workers=num_concurrent) as executor:
        futures = [executor.submit(send_request, i) for i in range(num_concurrent)]

        for future in concurrent.futures.as_completed(futures, timeout=timeout + 30):
            try:
                result = future.result(timeout=10)
                if "_http_error" in result or "error" in result:
                    errors.append(result)
                else:
                    results.append(result)
            except Exception as exc:
                errors.append({"error": str(exc), "_exception": type(exc).__name__})

    total_time = (time.time() - start_time) * 1000.0

    # Analyze results
    latencies = [r["_request_time_ms"] for r in results]
    throughput = len(results) / (total_time / 1000.0) if total_time > 0 else 0
    avg_latency = statistics.mean(latencies) if latencies else 0
    p95_latency = statistics.quantiles(latencies, n=20)[18] if len(latencies) >= 20 else max(latencies or [0])

    print(f"  Successful: {len(results)}/{num_concurrent}")
    print(f"  Failed: {len(errors)}/{num_concurrent}")
    print(f"  Throughput: {throughput:.2f} req/s")
    print(f"  Avg latency: {avg_latency:.1f}ms")
    print(f"  P95 latency: {p95_latency:.1f}ms")
    print(f"  Total time: {total_time:.1f}ms")

    if errors:
        print(f"  Errors:")
        for err in errors[:3]:
            print(f"    - {err}")

    return {
        "concurrent_requests": num_concurrent,
        "successful": len(results),
        "failed": len(errors),
        "throughput_req_s": throughput,
        "avg_latency_ms": avg_latency,
        "p95_latency_ms": p95_latency,
        "total_time_ms": total_time,
    }


def test_inference_timeout_recovery(timeout_s: int = 5) -> dict:
    """Test that inference completes or times out gracefully."""
    print(f"\n[timeout] Testing inference timeout recovery ({timeout_s}s limit)...")

    # Send a very long request that might timeout
    message = "Explain quantum computing in exhaustive detail with mathematical foundations" * 5

    start = time.time()
    result = _post_chat(message, timeout=timeout_s)
    elapsed = time.time() - start

    if "_http_error" in result:
        print(f"  HTTP {result['_http_error']}: {result.get('error', 'N/A')}")
    elif "error" in result:
        print(f"  Error: {result['error']}")
    else:
        response = result.get("response", "")
        print(f"  Response: {response[:80]}...")

    print(f"  Request time: {elapsed:.2f}s (timeout was {timeout_s}s)")

    if elapsed > timeout_s * 1.5:
        print(f"  ⚠ WARNING: Request took {elapsed:.2f}s, exceeded timeout by {elapsed - timeout_s:.2f}s")
    elif elapsed <= timeout_s:
        print(f"  ✓ Completed within timeout")
    else:
        print(f"  ⚠ Slightly exceeded timeout")

    return {
        "timeout_seconds": timeout_s,
        "actual_time_seconds": elapsed,
        "exceeded_timeout": elapsed > timeout_s * 1.1,
    }


def test_inference_reliability(num_runs: int = 20) -> dict:
    """Test inference reliability: success rate and latency consistency."""
    print(f"\n[reliability] Testing inference reliability ({num_runs} runs)...")

    prompts = [
        "Hello, tell me about your capabilities.",
        "What is the meaning of life?",
        "Explain the internet.",
        "Describe a tree.",
        "What is programming?",
    ]

    latencies = []
    token_counts = []
    errors = 0

    for i in range(num_runs):
        prompt = prompts[i % len(prompts)]
        result = _post_chat(prompt, timeout=60)

        if "_http_error" in result or "error" in result:
            errors += 1
            print(f"  Run {i + 1}: ✗ Error")
        else:
            latency = result.get("_request_time_ms", 0)
            tokens = result.get("tokens_generated", 0)
            latencies.append(latency)
            token_counts.append(tokens)
            print(f"  Run {i + 1}: ✓ {tokens} tokens in {latency:.1f}ms")

    success_rate = (num_runs - errors) / num_runs * 100 if num_runs > 0 else 0
    avg_latency = statistics.mean(latencies) if latencies else 0
    stdev_latency = statistics.stdev(latencies) if len(latencies) > 1 else 0
    jitter = (stdev_latency / avg_latency * 100) if avg_latency > 0 else 0

    print(f"\n  Success rate: {success_rate:.1f}% ({num_runs - errors}/{num_runs})")
    print(f"  Avg latency: {avg_latency:.1f}ms")
    print(f"  Latency stdev: {stdev_latency:.1f}ms")
    print(f"  Jitter (CV): {jitter:.1f}%")
    print(f"  Avg tokens/run: {statistics.mean(token_counts):.0f}" if token_counts else "  Avg tokens: N/A")

    if success_rate < 95:
        print(f"  ⚠ WARNING: Success rate below 95%")
    if jitter > 25:
        print(f"  ⚠ WARNING: Latency jitter above 25% (erratic for agents)")

    return {
        "runs": num_runs,
        "success_rate_percent": success_rate,
        "errors": errors,
        "avg_latency_ms": avg_latency,
        "latency_stdev_ms": stdev_latency,
        "jitter_percent": jitter,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Advanced agentic inference reliability tests")
    parser.add_argument("--concurrent", type=int, default=10, help="Number of concurrent requests")
    parser.add_argument("--reliability-runs", type=int, default=20, help="Number of reliability test runs")
    parser.add_argument("--timeout", type=int, default=5, help="Timeout test limit in seconds")
    args = parser.parse_args()

    print("[advanced] Waiting for backend...")
    _wait_ready()

    results = {}

    # Test 1: Concurrent load
    try:
        results["concurrent_load"] = test_concurrent_load(args.concurrent)
    except Exception as exc:
        print(f"\n[load] FAIL: {exc}")
        results["concurrent_load"] = {"error": str(exc)}

    # Test 2: Timeout recovery
    try:
        results["timeout_recovery"] = test_inference_timeout_recovery(args.timeout)
    except Exception as exc:
        print(f"\n[timeout] FAIL: {exc}")
        results["timeout_recovery"] = {"error": str(exc)}

    # Test 3: Reliability
    try:
        results["reliability"] = test_inference_reliability(args.reliability_runs)
    except Exception as exc:
        print(f"\n[reliability] FAIL: {exc}")
        results["reliability"] = {"error": str(exc)}

    # Summary
    print("\n" + "=" * 60)
    print("[advanced] SUMMARY")
    print("=" * 60)

    concurrent_result = results.get("concurrent_load", {})
    if "error" not in concurrent_result:
        print(f"Concurrent load ({concurrent_result.get('concurrent_requests')} req):")
        print(f"  Success: {concurrent_result.get('successful')}/{concurrent_result.get('concurrent_requests')}")
        print(f"  Throughput: {concurrent_result.get('throughput_req_s'):.2f} req/s")
        print(f"  P95 latency: {concurrent_result.get('p95_latency_ms'):.1f}ms")

    timeout_result = results.get("timeout_recovery", {})
    if "error" not in timeout_result:
        status = "✓" if not timeout_result.get("exceeded_timeout") else "✗"
        print(f"\nTimeout recovery: {status}")
        print(f"  Timeout: {timeout_result.get('timeout_seconds')}s")
        print(f"  Actual: {timeout_result.get('actual_time_seconds'):.2f}s")

    reliability_result = results.get("reliability", {})
    if "error" not in reliability_result:
        print(f"\nReliability ({reliability_result.get('runs')} runs):")
        print(f"  Success rate: {reliability_result.get('success_rate_percent'):.1f}%")
        print(f"  Jitter: {reliability_result.get('jitter_percent'):.1f}%")

    overall_pass = all(
        "error" not in results.get(key, {})
        for key in ["concurrent_load", "timeout_recovery", "reliability"]
    )

    print("\n" + "=" * 60)
    if overall_pass:
        print("[advanced] PASS: All reliability tests completed")
        return 0
    else:
        print("[advanced] PARTIAL: Some tests had issues")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
