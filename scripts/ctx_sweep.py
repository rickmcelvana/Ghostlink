"""Does the >=10 GB context cap actually protect against anything?

`get_ctx_size` caps context at 4096 for models of 10 GB and over, independent of the VRAM
tier. That table carries no measurement or comment, unlike `get_ngl`, which documents its.
On this machine the configured model is 12 GB, so the cap is always what applies and long
conversations get ~8 turns of context regardless of what `settings.json` asks for.

This sweeps context size against the real models and reports, for each: whether it loaded,
how long it took, and what prefill and decode cost. A cap that is protecting against a real
failure should show a load failure above it. A cap that is not should show nothing but
cost.

Deliberately launches `llama-server` directly rather than going through Ghostlink, so the
answer is about the model and the hardware, not about which settings happen to be on disk.
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
SCRATCH = os.path.join(os.environ.get("TEMP", ROOT), "ghostlink-ctx-sweep")
SERVER = os.path.join(ROOT, "third_party/llama.cpp/build/bin/Release/llama-server.exe")
MODELS_DIR = os.path.join(ROOT, "models")

# Set when a repeated sweep produced conflicting results at the same context size,
# which is what a shared-Vulkan nondeterministic allocation looks like.
FLAKY_NOTE = os.environ.get("GHOSTLINK_CTX_FLAKY_NOTE", "")

THREADS = 8
GPU_LAYERS = 24  # get_ngl's value for a sub-10GB model; 0 for >=10GB, see below

# Enough text that prefill cost is visible and comparable across context sizes.
PROMPT = (
    "A deployment pipeline validates a rollout manifest, checksums every artifact, and "
    "refuses to promote a build whose rollback target is unreachable. "
) * 30


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def wait_ready(port: int, proc, limit: int = 900) -> tuple[bool, str]:
    """Ready means /completion answers. Returns (ok, reason)."""
    deadline = time.time() + limit
    while time.time() < deadline:
        if proc.poll() is not None:
            return False, "process exited during load"
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=2):
                body = json.dumps({"prompt": "hi", "n_predict": 1, "stream": False}).encode()
                urllib.request.urlopen(
                    urllib.request.Request(
                        f"http://127.0.0.1:{port}/completion", data=body,
                        headers={"Content-Type": "application/json"},
                    ),
                    timeout=180,
                ).read()
                return True, ""
        except Exception:
            time.sleep(3)
    return False, "timed out waiting for load"


def load_error(log_path: str) -> str:
    """The most specific failure line in a load log, if any."""
    try:
        text = open(log_path, encoding="utf-8", errors="replace").read()
    except OSError:
        return ""
    for needle in (
        "ErrorOutOfDeviceMemory",
        "exceeds the available context size",
        "failed to allocate compute",
        "error loading model",
        "exiting due to model loading error",
    ):
        if needle in text:
            for line in text.splitlines():
                if needle in line:
                    return line.strip()[-110:]
    return ""


def trial(label: str, model_path: str, ctx: int, runs: int = 2) -> dict:
    size_gb = os.path.getsize(model_path) / 1e9
    ngl = 0 if size_gb >= 10.0 else GPU_LAYERS
    port = free_port()
    os.makedirs(SCRATCH, exist_ok=True)
    tag = f"{label}-ctx{ctx}"
    # Timestamped: an earlier version used a fixed name per (model, ctx), so repeat
    # sweeps silently overwrote each other. That is how "8192 is fine" and "8192 fails"
    # both appeared true at different times -- the second run had erased the first.
    stamp = time.strftime("%H%M%S")
    log_path = os.path.join(SCRATCH, f"{tag}-ctx{ctx}-{stamp}.log")
    cmd = [
        SERVER, "-m", model_path, "--alias", tag,
        "--host", "127.0.0.1", "--port", str(port),
        "--no-warmup", "-t", str(THREADS), "-c", str(ctx), "-np", "1",
        "-ngl", str(ngl), "-fa", "on", "-b", "512", "-ub", "128",
        "-ctk", "q8_0", "-ctv", "q8_0",
    ]
    started = time.time()
    log = open(log_path, "w", encoding="utf-8", errors="replace")
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)
    try:
        ok, reason = wait_ready(port, proc)
        if not ok:
            return {
                "label": label, "gb": size_gb, "ctx": ctx, "ngl": ngl,
                "loaded": False, "why": reason, "detail": load_error(log_path),
                "load_s": round(time.time() - started),
            }
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
            "label": label, "gb": size_gb, "ctx": ctx, "ngl": ngl,
            "loaded": True, "load_s": round(time.time() - started),
            "prompt_tps": statistics.median(pre), "decode_tps": statistics.median(dec),
        }
    except Exception as e:
        return {
            "label": label, "gb": size_gb, "ctx": ctx, "ngl": ngl,
            "loaded": False, "why": repr(e)[:70], "detail": load_error(log_path),
            "load_s": round(time.time() - started),
        }
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=30)
        log.close()
        # Confirm the port is actually free. Four orphaned llama-servers from repeated
        # sweeps once held ~8 GB of working set and took the machine down; nothing in the
        # terminate path reported that, because the process we spawned had already gone.
        for _ in range(20):
            probe = socket.socket()
            if probe.connect_ex(("127.0.0.1", port)) != 0:
                probe.close()
                break
            probe.close()
            time.sleep(0.5)
        else:
            print(
                f"  WARNING: port {port} still answering after terminating "
                f"{tag}; a llama-server may have leaked. Check for orphans before "
                "running more trials -- leaked model processes exhaust RAM.",
                file=sys.stderr,
            )


def main() -> int:
    if not os.path.exists(SERVER):
        print(f"error: no llama-server at {SERVER}", file=sys.stderr)
        return 2
    which = sys.argv[1] if len(sys.argv) > 1 else "27B"
    ctxs = [int(x) for x in (sys.argv[2] if len(sys.argv) > 2 else "4096,8192,16332").split(",")]

    if which == "27B":
        name = "Qwen3.8-27B-UD-IQ3_S.gguf"
    elif which == "30B":
        name = "Qwen3-Coder-30B-A3B-Instruct-Q3_K_M.gguf"
    elif which == "9B":
        name = "Qwopus3.5-9B-Coder-MTP-Q3_K_M.gguf"
    else:
        name = which if which.endswith(".gguf") else f"{which}.gguf"
    path = os.path.join(MODELS_DIR, name)
    if not os.path.exists(path):
        print(f"error: no model at {path}", file=sys.stderr)
        return 2

    gb = os.path.getsize(path) / 1e9
    # Mirror get_ctx_size's own policy instead of hardcoding a number: the >=10 GB cap is
    # 4096, the 5-10 GB cap is 8192, and under 5 GB there is no cap at all. An earlier
    # version printed "would cap at 4096" for every model, which is wrong for exactly the
    # model this branch cares about.
    cap = 4096 if gb >= 10.0 else (8192 if gb >= 5.0 else None)
    print(f"model: {name}  ({gb:.2f} GB)")
    print(f"context sizes under test: {ctxs}")
    print(
        f"get_ctx_size model_cap: {'none (uncapped)' if cap is None else cap}"
        f"   [vram tier not applied here; this sweeps the hardware limit]"
    )
    print()
    print(f"{'ctx':>7} {'loaded':>7} {'load s':>7} {'prefill':>9} {'decode':>8}  note")
    print("-" * 74)
    rows = []
    for ctx in ctxs:
        r = trial(name.replace(".gguf", ""), path, ctx)
        rows.append(r)
        if not r["loaded"]:
            note = (r.get("detail") or r.get("why") or "")[:34]
            print(f"{ctx:>7} {'NO':>7} {r['load_s']:>7} {'-':>9} {'-':>8}  {note}")
            continue
        pt = f"{r['prompt_tps']:.1f}" if r.get("prompt_tps") else "-"
        dt = f"{r['decode_tps']:.2f}" if r.get("decode_tps") else "-"
        print(f"{ctx:>7} {'yes':>7} {r['load_s']:>7} {pt:>9} {dt:>8}")

    ok = [r for r in rows if r["loaded"] and r.get("decode_tps")]
    print()
    if FLAKY_NOTE:
        print("  NOTE: repeat sweeps disagreed at some sizes. A single sweep cannot")
        print("        establish a safe maximum on a shared Vulkan iGPU.")
        print()
    if ok and len(ok) > 1:
        base = min(ok, key=lambda r: r["ctx"])
        for r in ok:
            if r is base:
                continue
            print(
                f"  ctx {r['ctx']} vs {base['ctx']}: "
                f"{r['decode_tps'] / base['decode_tps']:.2f}x decode, "
                f"{r['prompt_tps'] / base['prompt_tps']:.2f}x prefill"
            )
        print()
        top = max(r["ctx"] for r in ok)
        print(f"  highest context that served this run: {top}")
        cap_txt = "none (uncapped)" if cap is None else str(cap)
        print(f"  get_ctx_size model_cap:               {cap_txt}")
        if FLAKY_NOTE:
            print("  => NOT a safe maximum: repeats disagreed. Treat this run as one sample.")
        elif cap is None:
            print("  => this model is not capped by size, so nothing is being withheld")
        elif top > cap:
            print("  => the cap is costing context on this hardware, at no measured cost")
        else:
            print("  => the cap is protective on this hardware")
    print("\nA '-' means llama.cpp did not report that field. Reported, not imputed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
