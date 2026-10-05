"""Measure every local model against this machine, so the default is a fact not a guess.

Runs after the prompt work in this series: the fixed prompt overhead was worth 2,767
tokens, but the measurement above showed a five-word question still taking 102 seconds --
which no prompt change can explain. The cause was the *model* being served: a 12 GB model
CPU-bound at 15 tok/s prefill.

So this enumerates what is actually on disk and reports decode and prefill for each.
The point is to answer "which model should Ghostlink serve here by default", and to make
the answer reproducible rather than a preference expressed in settings.json.
"""
import json
import os
import socket
import statistics
import subprocess
import sys
import time
import urllib.request

ROOT = r"C:\Users\rwill\Ghostlink"
SCRATCH = os.path.join(os.environ.get("TEMP", ROOT), "ghostlink-model-survey")
SERVER = os.path.join(ROOT, "third_party/llama.cpp/build/bin/Release/llama-server.exe")
MODELS_DIR = os.path.join(ROOT, "models")

# Matches what get_ngl/get_ctx_size actually choose, so the survey reflects the code
# path rather than an idealised one.
THREADS = 8
CTX = 8192
GPU_LAYERS = 24

PROMPT = (
    "A deployment pipeline validates a rollout manifest, checksums every artifact, and "
    "refuses to promote a build whose rollback target is unreachable. "
) * 40


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def wait_ready(port: int, proc, limit: int = 900) -> bool:
    """Ready means /completion answers, not that the port is bound.

    llama-server binds while loading and serves 503 until it is done.
    """
    deadline = time.time() + limit
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
        except Exception:
            time.sleep(3)
    return False


def measure(label: str, model_path: str, runs: int = 2) -> dict:
    size_gb = os.path.getsize(model_path) / 1e9
    # get_ngl forces 0 for models >= 10 GB on this hardware (measured: partial offload
    # is 2.4x slower than the CPU there). Mirror that rather than testing an ideal.
    ngl = 0 if size_gb >= 10.0 else GPU_LAYERS
    port = free_port()
    cmd = [
        SERVER, "-m", model_path, "--alias", label,
        "--host", "127.0.0.1", "--port", str(port),
        "--no-warmup", "-t", str(THREADS), "-c", str(CTX), "-np", "1",
        "-ngl", str(ngl), "-fa", "on", "-b", "512", "-ub", "128",
        "-ctk", "q8_0", "-ctv", "q8_0",
    ]
    os.makedirs(SCRATCH, exist_ok=True)
    log = open(os.path.join(SCRATCH, f"{label}.log"), "w", encoding="utf-8", errors="replace")
    started = time.time()
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)
    try:
        if not wait_ready(port, proc):
            return {"label": label, "gb": size_gb, "ngl": ngl, "error": "did not start"}
        body = json.dumps({
            "prompt": PROMPT, "stream": False, "max_tokens": 24,
            "temperature": 0.0, "cache_prompt": False,
        }).encode()
        pre, dec = [], []
        for _ in range(runs):
            d = json.loads(urllib.request.urlopen(
                urllib.request.Request(
                    f"http://127.0.0.1:{port}/completion", data=body,
                    headers={"Content-Type": "application/json"},
                ),
                timeout=900,
            ).read())
            tm = d.get("timings", {})
            pre.append(tm.get("prompt_per_second"))
            dec.append(tm.get("predicted_per_second"))
        return {
            "label": label, "gb": size_gb, "ngl": ngl,
            "load_s": round(time.time() - started),
            "prompt_tps": statistics.median(pre),
            "decode_tps": statistics.median(dec),
        }
    except Exception as e:
        return {"label": label, "gb": size_gb, "ngl": ngl, "error": repr(e)[:90]}
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=25)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()


def main() -> int:
    if not os.path.exists(SERVER):
        print(f"error: no llama-server at {SERVER}", file=sys.stderr)
        return 2
    models = sorted(
        f for f in os.listdir(MODELS_DIR)
        if f.endswith(".gguf") and "embed" not in f.lower()
    )
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 2
    print(f"models on disk: {len(models)}   (threads={THREADS} ctx={CTX})")
    print("ngl mirrors get_ngl: 0 for models >= 10 GB, else 24\n")
    print(f"{'model':44} {'GB':>5} {'ngl':>4} {'load s':>7} {'prefill':>9} {'decode':>8}")
    print("-" * 82)
    rows = []
    for m in models:
        r = measure(m.replace(".gguf", ""), os.path.join(MODELS_DIR, m), runs)
        rows.append(r)
        if r.get("error"):
            print(f"{r['label'][:43]:44} {r['gb']:5.1f} {r['ngl']:4} {'-':>7} {'-':>9} {'-':>8}   {r['error'][:30]}")
            continue
        pt = f"{r['prompt_tps']:.1f}" if r.get("prompt_tps") else "-"
        dt = f"{r['decode_tps']:.2f}" if r.get("decode_tps") else "-"
        print(f"{r['label'][:43]:44} {r['gb']:5.1f} {r['ngl']:4} {r['load_s']:7} {pt:>9} {dt:>8}")

    ok = [r for r in rows if not r.get("error") and r.get("decode_tps")]
    if ok:
        best = max(ok, key=lambda r: r["decode_tps"])
        print(f"\nfastest decode: {best['label']}  ({best['decode_tps']:.2f} tok/s)")
        cur = next((r for r in ok if "27B" in r["label"]), None)
        if cur and best["label"] != cur["label"]:
            print(f"currently served model {cur['label']} decodes at {cur['decode_tps']:.2f} tok/s")
            print(f"  -> {cur['decode_tps'] / best['decode_tps']:.1f}x slower than {best['label']}")
    print("\nA '-' means llama.cpp did not report that field. Reported, not imputed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
