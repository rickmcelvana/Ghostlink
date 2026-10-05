"""A/B the llama.cpp knobs the inference audit claims are worth having.

Single node, one model, one prompt shape, one variable at a time. Reports prefill and
decode separately, because the two respond to different levers and averaging them is how
a change looks like an improvement when it only moved one half.

Every run launches its own `llama-server`, so nothing is inherited from a previous
configuration and nothing depends on Ghostlink being up.

Deliberately measures the raw server, not Ghostlink. The claim under test is about
llama.cpp's flags; routing through the product would add its own prompt assembly and
history budget on top and make the comparison unattributable.
"""
import json
import os
import socket
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRATCH = os.path.join(os.environ.get("TEMP", ROOT), "ghostlink-knob-ab")
SERVER = os.path.join(ROOT, "third_party/llama.cpp/build/bin/Release/llama-server.exe")
MODEL = os.path.join(ROOT, "models/Llama-3.2-3B-Instruct-Q3_K_M.gguf")

FILLER = (
    "Explain how a deployment pipeline validates manifests, checksums and signed "
    "release artifacts before promoting a build between environments. "
)


def build_prompt(target_chars: int) -> str:
    """A user turn of roughly `target_chars`, deterministic across runs."""
    body = FILLER
    reps = max(1, target_chars // len(body))
    return "Summarize the deployment process described below.\n\n" + (body * reps)


def wait_ready(port: int, proc, timeout_s: int = 600) -> bool:
    """Ready means /completion answers, not merely that the socket is bound.

    llama-server binds the port while the model is still loading and answers HTTP 503
    until it is done. Polling the socket therefore reported "ready" immediately and the
    first measured request died with a 503 -- which is how the first run of this script
    produced no numbers at all.
    """
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if proc.poll() is not None:
            return False
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=2):
                body = json.dumps({"prompt": "hi", "n_predict": 1, "stream": False}).encode()
                urllib.request.urlopen(
                    urllib.request.Request(
                        f"http://127.0.0.1:{port}/completion", data=body,
                        headers={"Content-Type": "application/json"},
                    ),
                    timeout=120,
                ).read()
                return True
        except urllib.error.HTTPError:
            time.sleep(3)  # 503 = still loading
        except OSError:
            time.sleep(2)
    return False


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def run_once(port: int, prompt: str, max_tokens: int) -> dict:
    """One buffered completion, timing the whole request."""
    body = json.dumps(
        {
            "prompt": prompt,
            "stream": False,
            "max_tokens": max_tokens,
            "temperature": 0.0,
            "cache_prompt": False,
        }
    ).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/completion", data=body,
        headers={"Content-Type": "application/json"},
    )
    t0 = time.perf_counter()
    data = json.loads(urllib.request.urlopen(req, timeout=600).read())
    wall_ms = (time.perf_counter() - t0) * 1000.0
    timings = data.get("timings", {})
    prompt_n = timings.get("prompt_n")
    prompt_per_s = timings.get("prompt_per_second")
    predicted_n = timings.get("predicted_n")
    predicted_per_s = timings.get("predicted_per_second")
    return {
        "wall_ms": wall_ms,
        "prompt_tokens": prompt_n,
        "prompt_tps": prompt_per_s,
        "decode_tokens": predicted_n,
        "decode_tps": predicted_per_s,
    }


def median(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def launch_and_measure(label: str, extra_args, runs: int, prompt_chars: int, max_tokens: int):
    port = free_port()
    cmd = [
        SERVER, "-m", MODEL, "--host", "127.0.0.1", "--port", str(port),
        "--no-warmup", "-t", "4", "-c", "8192", "-np", "1",
    ] + extra_args
    os.makedirs(SCRATCH, exist_ok=True)
    log_path = os.path.join(SCRATCH, f"ab_{label}.log")
    log = open(log_path, "w", encoding="utf-8", errors="replace")
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)
    try:
        if not wait_ready(port, proc):
            err = ""
            try:
                proc.terminate()
                err = log.read()[-400:]
            except Exception:
                pass
            return {"label": label, "args": " ".join(extra_args), "error": err or "did not start"}

        prompt = build_prompt(prompt_chars)
        # Warm the slot so the first measured run is not paying a one-off page-in.
        run_once(port, prompt, 8)

        samples = []
        for _ in range(runs):
            try:
                samples.append(run_once(port, prompt, max_tokens))
            except urllib.error.HTTPError as e:
                # 503 here means the server began reloading, not that the config is bad.
                samples.append({"error": f"HTTP {e.code}"})
            except Exception as e:  # a single failure must not void the run
                samples.append({"error": str(e)})

        ok = [s for s in samples if "error" not in s]
        return {
            "label": label,
            "args": " ".join(extra_args) or "(none)",
            "runs": len(ok),
            "errors": [s["error"] for s in samples if "error" in s],
            "prompt_tokens": median([s.get("prompt_tokens") for s in ok]),
            "prompt_tps": median([s.get("prompt_tps") for s in ok]),
            "decode_tokens": median([s.get("decode_tokens") for s in ok]),
            "decode_tps": median([s.get("decode_tps") for s in ok]),
            "wall_ms": median([s.get("wall_ms") for s in ok]),
        }
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()


CONFIGS = [
    # Baseline: what Ghostlink launches today on this machine (see default_perf_args).
    ("baseline", ["-fa", "on", "-b", "512", "-ub", "128", "-ctk", "q8_0", "-ctv", "q8_0", "-ngl", "24"]),
    # The memo's claim: larger prefill batch is the difference between ~70 and 300+ tok/s.
    ("b2048ub512", ["-fa", "on", "-b", "2048", "-ub", "512", "-ctk", "q8_0", "-ctv", "q8_0", "-ngl", "24"]),
    # Batch alone, to separate -b from -ub.
    ("b2048ub128", ["-fa", "on", "-b", "2048", "-ub", "128", "-ctk", "q8_0", "-ctv", "q8_0", "-ngl", "24"]),
    # Flash attention off, to confirm it is actually doing something.
    ("nofa", ["-b", "512", "-ub", "128", "-ngl", "24"]),
    # q8_0 vs f16 KV: the quality-vs-speed claim.
    ("kvf16", ["-fa", "on", "-b", "512", "-ub", "128", "-ctk", "f16", "-ctv", "f16", "-ngl", "24"]),
    # q4_0 KV, the aggressive end of the same tradeoff.
    ("kvq4", ["-fa", "on", "-b", "512", "-ub", "128", "-ctk", "q4_0", "-ctv", "q4_0", "-ngl", "24"]),
]


def main() -> int:
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    prompt_chars = int(sys.argv[2]) if len(sys.argv) > 2 else 6000
    print(f"model : {os.path.basename(MODEL)}")
    print(f"runs  : {runs} per config (after 1 warmup)")
    print(f"prompt: ~{prompt_chars} chars\n")
    print(f"{'config':12} {'prefill tok/s':>14} {'decode tok/s':>13} {'wall ms':>9} {'err':>4}")
    print("-" * 58)
    rows = []
    for label, args in CONFIGS:
        r = launch_and_measure(label, args, runs, prompt_chars, 32)
        rows.append(r)
        if r.get("error"):
            print(f"{label:12} {'-':>14} {'-':>13} {'-':>9} {'FAIL':>4}")
            print(f"             {r['error'].strip()[:200]}")
            continue
        pt = f"{r['prompt_tps']:.1f}" if r["prompt_tps"] else "-"
        dt = f"{r['decode_tps']:.2f}" if r["decode_tps"] else "-"
        wm = f"{r['wall_ms']:.0f}" if r["wall_ms"] else "-"
        print(f"{label:12} {pt:>14} {dt:>13} {wm:>9} {len(r.get('errors') or []):>4}")

    base = next((r for r in rows if r["label"] == "baseline" and not r.get("error")), None)
    if base and base["prompt_tps"]:
        print(f"\nprefill relative to baseline ({base['prompt_tps']:.1f} tok/s):")
        for r in rows:
            if r.get("error") or r["label"] == "baseline":
                continue
            if r["prompt_tps"]:
                print(f"  {r['label']:12} {r['prompt_tps'] / base['prompt_tps']:5.2f}x prefill"
                      f"   {(base['decode_tps'] / r['decode_tps']):5.2f}x decode"
                      if base["decode_tps"] and r["decode_tps"] else
                      f"  {r['label']:12} {r['prompt_tps'] / base['prompt_tps']:5.2f}x prefill")
    print("\nA '-' means llama.cpp did not report that field. That is reported, not imputed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
