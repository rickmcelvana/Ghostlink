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


# A fixed reference passage, identical for every model, scored by mean per-token logprob.
#
# NEGATIVE RESULT, kept deliberately: this was added to give the speed table a quality
# axis, and it does not work. Measured across the local models:
#
#     Llama-3.2-1B                 -2.094
#     Llama-3.2-3B                 -2.219
#     Qwopus3.5-9B-Coder-MTP        -1.937
#     MiMo-V2.6-Distill-Qwen-9B     -2.287
#
# A 20x range in parameter count produced a 0.35 nat spread, and the ordering is not
# monotonic in size -- the 1B beats the 3B, and two same-size-class 9B models differ by
# 0.35. Mean logprob on one in-domain English passage measures how fluent a model finds
# *that passage*, not capability: a code-specialised model reads prose fluently.
#
# So it stays in the script, printing the number and this warning, rather than being
# deleted and re-invented later. Anyone tempted to rank models by it should see that it
# was tried and why it was rejected. Choosing a default model on quality needs a task
# benchmark or human evaluation; neither is available offline here, and a proxy that does
# not discriminate is worse than no number at all.
REFERENCE_TEXT = (
    "The deployment pipeline validates a rollout manifest before promoting a build. "
    "It verifies that every artifact has a matching checksum, that the configuration "
    "schema has not drifted, and that the rollback target remains reachable. A failed "
    "check halts promotion rather than degrading silently, because a partial rollout "
    "is harder to reason about than a delayed one. "
)


def measure_reference_logprob(port: int) -> float | None:
    """Mean per-token logprob over REFERENCE_TEXT, or None if not reported.

    Requests n_predict=1 with logprobs so llama-server returns
    `completion_probabilities`, and averages the logprob of the tokens it generated.

    Caveat worth stating: this is the model's confidence on its *own* continuation, not
    a likelihood assigned to the reference. It is a consistent comparison across models
    on identical text, which is the property needed here, and it is not a claim about
    answer correctness on any task.
    """
    body = json.dumps({
        "prompt": REFERENCE_TEXT, "n_predict": 1, "stream": False, "logprobs": 1,
        "temperature": 0.0, "cache_prompt": False,
    }).encode()
    try:
        d = json.loads(urllib.request.urlopen(
            urllib.request.Request(
                f"http://127.0.0.1:{port}/completion", data=body,
                headers={"Content-Type": "application/json"},
            ),
            timeout=300,
        ).read())
    except Exception:
        return None
    probs = d.get("completion_probabilities") or []
    vals = [p.get("logprob") for p in probs if p.get("logprob") is not None]
    return statistics.median(vals) if vals else None


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
            "ref_logprob": measure_reference_logprob(port),
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
    print(
        f"{'model':40} {'GB':>5} {'ngl':>4} {'load s':>7} {'prefill':>9} "
        f"{'decode':>8} {'ref logprob':>12}"
    )
    print("-" * 90)
    rows = []
    for m in models:
        r = measure(m.replace(".gguf", ""), os.path.join(MODELS_DIR, m), runs)
        rows.append(r)
        if r.get("error"):
            print(
                f"{r['label'][:39]:40} {r['gb']:5.1f} {r['ngl']:4} {'-':>7} {'-':>9} "
                f"{'-':>8} {'-':>12}   {r['error'][:28]}"
            )
            continue
        pt = f"{r['prompt_tps']:.1f}" if r.get("prompt_tps") else "-"
        dt = f"{r['decode_tps']:.2f}" if r.get("decode_tps") else "-"
        lp = f"{r['ref_logprob']:.3f}" if r.get("ref_logprob") is not None else "-"
        print(
            f"{r['label'][:39]:40} {r['gb']:5.1f} {r['ngl']:4} {r['load_s']:7} "
            f"{pt:>9} {dt:>8} {lp:>12}"
        )

    ok = [r for r in rows if not r.get("error") and r.get("decode_tps")]
    if ok:
        best = max(ok, key=lambda r: r["decode_tps"])
        print(f"\nfastest decode: {best['label']}  ({best['decode_tps']:.2f} tok/s)")
        cur = next((r for r in ok if "27B" in r["label"]), None)
        if cur and best["label"] != cur["label"] and cur["decode_tps"]:
            print(
                f"currently served model {cur['label']} decodes at {cur['decode_tps']:.2f}"
                f" tok/s"
            )
            print(
                f"  -> {best['decode_tps'] / cur['decode_tps']:.1f}x faster than "
                f"{cur['label']}"
            )
    print("\nA '-' means llama.cpp did not report that field. Reported, not imputed.")
    print(
        "\nref logprob -- NEGATIVE RESULT, do not rank models by this column.\n"
        "Median per-token logprob on one fixed English passage. Measured spread across\n"
        "these models was 0.35 nats over a 20x parameter range, and the ordering is not\n"
        "monotonic in size (the 1B beats the 3B; two 9B-class models differ by 0.35). It\n"
        "measures fluency on that passage, not capability. Kept visible so it is not\n"
        "re-invented. Use a task benchmark or human eval to choose on quality."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
