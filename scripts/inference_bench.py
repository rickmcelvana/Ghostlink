#!/usr/bin/env python3
"""Measure Ghostlink inference, separating the three costs that matter.

Why this exists
---------------
A single tok/s number over a chat request averages two unrelated things:

  * **prefill** -- evaluating the prompt. Compute-bound, batches well, and can
    reach hundreds of tok/s.
  * **decode**  -- generating the reply. Memory-bandwidth-bound, does not batch,
    and on integrated hardware can be single digits.

Averaged together they move for reasons that have nothing to do with each other,
which makes them useless for deciding whether a change helped. Before this, the
server reported decode throughput and TTFT but never read llama.cpp's prompt
timings at all, so prefill was unmeasurable and every local-vs-RPC comparison
had to fall back on end-to-end latency -- which conflates the two.

What it reports
---------------
  ttft_ms               time to first token
  prompt_tokens         prompt size (the thing a long history inflates)
  prompt_ms             prefill wall time
  prompt_tps            prefill throughput
  decode_tps            generation throughput
  total_ms              end to end

A missing measurement is printed as `-`, never as `0`. "Not measured" and
"measured as zero" must not look alike, and a fabricated number in a benchmark is
worse than an honest gap.

Usage
-----
    python scripts/inference_bench.py --runs 3
    python scripts/inference_bench.py --runs 3 --prompt-tokens 2000 --label "sliding window"
    python scripts/inference_bench.py --local-only     # no RPC, no allowlist proxy
    python scripts/inference_bench.py --compare local,rpc

`--compare` is the point of the tool: it refuses to declare a winner when either
side is unmeasured, and it reports the prefill/decode split rather than one
averaged figure.
"""

from __future__ import annotations

import argparse
import json
import os
import ssl
import statistics
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Optional

DEFAULT_BASE = os.environ.get("GHOSTLINK_BENCH_URL", "https://127.0.0.1:8003")
TOKEN_PATH = os.environ.get("GHOSTLINK_API_KEY_FILE", "api_key.txt")


def fmt(value: Optional[float], unit: str = "", width: int = 0) -> str:
    """Render a measurement, or '-' when there isn't one.

    Deliberately never renders a missing value as 0.0: a benchmark that reports
    "0 tok/s" for something it did not measure is worse than one that admits the
    gap, because 0 reads as a result.
    """
    if value is None:
        return "-".rjust(width)
    if unit:
        return f"{value:.2f}{unit}".rjust(width)
    return f"{value:.2f}".rjust(width)


@dataclass
class Sample:
    ttft_ms: Optional[float] = None
    prompt_tokens: Optional[int] = None
    prompt_ms: Optional[float] = None
    prompt_tps: Optional[float] = None
    decode_tps: Optional[float] = None
    tokens_generated: Optional[int] = None
    total_ms: float = 0.0
    error: Optional[str] = None
    label: str = ""
    # Server-lifetime TTFT p50, reported once for context and never attributed to a
    # single run.
    rolling_ttft_p50: Optional[float] = None


@dataclass
class Result:
    label: str
    samples: list[Sample] = field(default_factory=list)

    def _vals(self, attr: str) -> list[float]:
        return [getattr(s, attr) for s in self.samples if getattr(s, attr) is not None]

    def median(self, attr: str) -> Optional[float]:
        vals = self._vals(attr)
        return statistics.median(vals) if vals else None

    @property
    def ok(self) -> int:
        return sum(1 for s in self.samples if s.error is None)

    @property
    def measured_prompt(self) -> int:
        return len(self._vals("prompt_tps"))


def build_prompt(target_tokens: int) -> str:
    """A prompt of roughly `target_tokens` tokens of real prose.

    Filler is drawn from the same text repeatedly rather than random words: token
    throughput depends on how predictable the input is, and lorem-like repetition
    would flatter prefill relative to a real conversation.
    """
    base = (
        "The deployment pipeline validates the rollout manifest before promoting a "
        "build. It checks that every artifact has a matching checksum, that the "
        "configuration schema has not drifted, and that the rollback target is still "
        "reachable. A failed check halts promotion rather than degrading silently, "
        "because a partial rollout is harder to reason about than a delayed one. "
        "The on-call engineer reviews any manifest that fails twice in a row. "
    )
    text = base
    while len(text.split()) < target_tokens:
        text += base
    return text


# A deliberately tiny prompt, used to measure per-request fixed overhead.
#
# Added because the harness could not see a 3,800-token constant: every prompt it
# generated was thousands of tokens of filler, where fixed overhead is noise. Real
# chat turns are often a sentence long, and there the overhead *is* the latency.
#
# Kept tiny and literal rather than generated, so the token count is stable across
# runs and the number is comparable over time.
OVERHEAD_PROMPT = "Reply with exactly: OK"


def measure_overhead(bench: "Bench", runs: int = 5) -> dict:
    """Prompt tokens and TTFT for a five-word request.

    This is the number that should be watched for regressions in prompt assembly. It is
    reported separately from prefill throughput on purpose: throughput can be healthy
    while the constant is enormous, which is exactly the failure this found.
    """
    for _ in range(2):
        bench.run_once(OVERHEAD_PROMPT, "overhead-warmup")
    samples = []
    for _ in range(runs):
        s = bench.run_once(OVERHEAD_PROMPT, "overhead")
        s.ttft_ms = bench._stream_once(OVERHEAD_PROMPT)
        samples.append(s)
    ok = [s for s in samples if s.error is None]
    return {
        "prompt_tokens": median_of(s.prompt_tokens for s in ok),
        "ttft_ms": median_of(s.ttft_ms for s in ok),
        "errors": [s.error for s in samples if s.error],
        "runs": len(ok),
    }


def median_of(values):
    vals = sorted(v for v in values if v is not None)
    return vals[len(vals) // 2] if vals else None


class Bench:
    def __init__(self, base: str, token: str, timeout: int = 600):
        self.base = base.rstrip("/")
        self.token = token
        self.ctx = ssl.create_default_context()
        self.ctx.check_hostname = False
        self.ctx.verify_mode = ssl.CERT_NONE
        self.timeout = timeout

    def _post(self, path: str, body: dict) -> dict:
        req = urllib.request.Request(
            f"{self.base}{path}",
            data=json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=self.timeout, context=self.ctx) as r:
            return json.loads(r.read())

    def wait_ready(self, seconds: int = 240) -> bool:
        for _ in range(seconds):
            try:
                req = urllib.request.Request(
                    f"{self.base}/health", headers={"Authorization": f"Bearer {self.token}"}
                )
                urllib.request.urlopen(req, timeout=5, context=self.ctx).read()
                return True
            except Exception:
                time.sleep(1)
        return False

    def describe(self) -> dict:
        """What is actually running, so a number is never reported without context."""
        out = {"model": None, "backend": None, "distributed": None, "mcp_servers": []}
        try:
            req = urllib.request.Request(
                f"{self.base}/api/models", headers={"Authorization": f"Bearer {self.token}"}
            )
            models = json.loads(urllib.request.urlopen(req, timeout=15, context=self.ctx).read())
            items = models.get("models") or []
            if isinstance(models, dict) and models.get("current_model"):
                out["model"] = models["current_model"]
            elif items:
                out["model"] = items[0].get("name") or items[0].get("id")
        except Exception:
            pass
        try:
            req = urllib.request.Request(
                f"{self.base}/api/mcp/servers", headers={"Authorization": f"Bearer {self.token}"}
            )
            servers = json.loads(urllib.request.urlopen(req, timeout=15, context=self.ctx).read())
            out["mcp_servers"] = [
                s["name"] for s in (servers.get("servers") or []) if s.get("connected")
            ]
        except Exception:
            pass
        return out

    def run_once(self, prompt: str, label: str) -> Sample:
        s = Sample(label=label)
        started = time.time()
        try:
            body = {"message": prompt, "stream": False, "max_tokens": 96}
            r = self._post("/api/inference/chat", body)
        except urllib.error.HTTPError as e:
            s.error = f"HTTP {e.code}"
            return s
        except Exception as e:
            s.error = repr(e)[:80]
            return s
        s.total_ms = (time.time() - started) * 1000.0

        m = r.get("metrics") or {}
        # Prefer the response's own fields; fall back to metrics for older builds.
        s.prompt_tokens = r.get("prompt_tokens", m.get("prompt_tokens"))
        s.prompt_ms = r.get("prompt_ms", m.get("prompt_ms"))
        s.prompt_tps = r.get("prompt_tokens_per_sec", m.get("prompt_tokens_per_sec"))
        s.decode_tps = r.get("decode_tokens_per_sec", m.get("decode_tokens_per_sec"))
        s.tokens_generated = r.get("tokens_generated")
        # TTFT is only observable on the streaming path -- a buffered response
        # arrives after generation is already over. The server records it per
        # streamed turn and exposes a rolling p50/p95, so that is what a non-streaming
        # probe can honestly report. The harness runs a streaming warmup first so the
        # rolling window is populated before the measured runs.
        # A buffered response cannot carry its own TTFT, so this stays None and the
        # per-run figure comes from the streaming probe in `measure`.
        #
        # The server's rolling ttft_p50_ms is deliberately NOT used here: it is a
        # lifetime figure that belongs to no particular run, and attributing it to
        # each run made three identical rows that all looked measured.
        s.ttft_ms = m.get("ttft_ms")
        if s.ttft_ms is not None and s.ttft_ms < 5.0:
            # The server seeds TTFT with 0.0 rather than a null, so an unpopulated
            # placeholder has to be turned back into "not measured" here.
            s.ttft_ms = None
        s.rolling_ttft_p50 = m.get("ttft_p50_ms")

        # A degraded backend returns an error string in `response` rather than a
        # non-2xx. Treating that as a measurement would report the failure text's
        # whitespace count as a token count.
        resp = r.get("response")
        if isinstance(resp, str) and ("Native error:" in resp or "error sending request" in resp):
            s.error = "degraded backend"
        if not r.get("real_inference", True):
            s.error = s.error or "not real inference"
        return s

    def _stream_once(self, prompt: str) -> Optional[float]:
        """One streamed request, returning its measured TTFT in ms.

        Needed because TTFT is unobservable on a buffered response. The server
        records it per streamed turn; this populates that rolling window so the
        measured runs have something to report.
        """
        req = urllib.request.Request(
            f"{self.base}/api/inference/chat",
            data=json.dumps({"message": prompt, "stream": True, "max_tokens": 24}).encode(),
            headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"},
        )
        try:
            # Start the clock BEFORE the request, not after the headers arrive. TTFT
            # is measured from the caller's perspective -- it includes the prefill of
            # a large prompt, which is exactly the cost worth knowing. Timing from the
            # first response byte measures only the SSE flush and reports ~0, which
            # is the kind of plausible-looking nonsense this tool exists to avoid.
            started = time.time()
            with urllib.request.urlopen(req, timeout=self.timeout, context=self.ctx) as r:
                for raw in r:
                    line = raw.decode("utf-8", "replace").strip()
                    if not line.startswith("data:"):
                        continue
                    body = line[5:].strip()
                    if not body or body == "[DONE]":
                        continue
                    try:
                        ev = json.loads(body)
                    except json.JSONDecodeError:
                        continue
                    if ev.get("token"):
                        return (time.time() - started) * 1000.0
        except Exception:
            return None
        return None

    def measure(self, label: str, runs: int, prompt_tokens: int, warmup: int = 1) -> Result:
        prompt = build_prompt(prompt_tokens)
        for _ in range(warmup):
            self.run_once(prompt, f"{label}-warmup")
        # Populate the server's rolling TTFT window.
        # Populate the server's rolling window, then discard: the per-run figure is
        # measured below against the real prompt.
        for _ in range(2):
            self._stream_once(build_prompt(64))
        res = Result(label=label)
        for i in range(runs):
            s = self.run_once(prompt, label)
            # Measure this run's own TTFT on the streaming path, with the same prompt.
            # The buffered run above already warmed the slot, so this is not paying a
            # cold-start cost the measured number would otherwise hide.
            s.ttft_ms = self._stream_once(prompt)
            res.samples.append(s)
            status = "ok" if s.error is None else s.error
            print(
               (f"  [{label}] run {i + 1}/{runs}: {status:22} "
                 f"prompt={fmt(s.prompt_tps, ' tok/s', 12)} "
                 f"decode={fmt(s.decode_tps, ' tok/s', 12)} "
                 f"ttft={fmt(s.ttft_ms, ' ms', 12)}"),
                flush=True,
            )
        return res


def report(results: list[Result], context: dict) -> None:
    print()
    print("=" * 78)
    print("INFERENCE MEASUREMENT — prefill and decode reported separately")
    print("=" * 78)
    if context.get("model"):
        print(f"  model      : {context['model']}")
    if context.get("mcp_servers"):
        print(f"  mcp servers: {', '.join(context['mcp_servers'])}")
    roll = [s.rolling_ttft_p50 for r in results for s in r.samples if s.rolling_ttft_p50]
    roll = [v for v in roll if v and v > 5.0]
    if roll:
        print(
            f"  ttft p50    : {statistics.median(roll):.0f} ms "
            f"(server-lifetime, all streamed turns — context only, not a per-run figure)"
        )
    print()
    header = (
        f"{'run':<18}{'ok':>4}{'prompt tok/s':>16}{'decode tok/s':>16}"
        f"{'ttft ms':>12}{'prompt tok':>12}"
    )
    print(header)
    print("-" * len(header))
    for r in results:
        print(
            f"{r.label:<18}{r.ok:>4}/{len(r.samples):<3}"
            f"{fmt(r.median('prompt_tps'), '', 16)}"
            f"{fmt(r.median('decode_tps'), '', 16)}"
            f"{fmt(r.median('ttft_ms'), '', 12)}"
            f"{fmt(r.median('prompt_tokens'), '', 12)}"
        )
    print()
    for r in results:
        if r.measured_prompt == 0:
            print(
                f"  NOTE: {r.label} reported no prompt timings. This build of "
                f"llama.cpp did not send them, so prefill is unmeasured — not zero."
            )
        errs = [s.error for s in r.samples if s.error]
        if errs:
            uniq = sorted(set(errs))
            print(f"  NOTE: {r.label} had failures: {', '.join(uniq)}")


def compare(a: Result, b: Result) -> None:
    """Compare two configurations, refusing to guess when data is missing."""
    print()
    print("-" * 78)
    print(f"COMPARISON: {a.label} vs {b.label}")
    print("-" * 78)

    for metric, unit, better in (
        ("decode_tps", " tok/s", "higher"),
        ("prompt_tps", " tok/s", "higher"),
        ("ttft_ms", " ms", "lower"),
    ):
        av: Optional[float] = a.median(metric)
        bv: Optional[float] = b.median(metric)
        if av is None or bv is None:
            which = []
            if av is None:
                which.append(f"{a.label}")
            if bv is None:
                which.append(f"{b.label}")
            print(
                f"  {metric:<14} INCONCLUSIVE — not measured on "
                f"{' and '.join(which)}. Not guessing."
            )
            continue
        delta = bv - av
        pct = (delta / av * 100.0) if av else float("inf")
        winner = b.label if ((delta > 0) == (better == "higher")) else a.label
        print(
            f"  {metric:<14} {a.label}={av:.2f}{unit}  {b.label}={bv:.2f}{unit}  "
            f"delta={delta:+.2f} ({pct:+.1f}%)  better: {winner}"
        )

    # The headline check: is distributing a model that fits locally costing anything?
    # Bound to locals first -- the truthiness guards above do not narrow the type
    # for a second call, and an Optional[float] reaching a comparison is a crash
    # waiting for a run where prefill happens to be unmeasured.
    ad = a.median("decode_tps")
    bd = b.median("decode_tps")
    if ad is not None and ad > 0.0 and bd is not None and bd > 0.0:
        if bd < ad * 0.97:
            print()
            print(
                f"  FINDING: {b.label} decodes {ad / bd:.2f}x slower than {a.label} "
                f"({(1 - bd / ad) * 100:.0f}% slower). If this model fits in one "
                f"device, tensor-splitting it across a network costs a round trip "
                f"per token and should not be the default."
            )
        elif bd > ad * 1.03:
            print()
            print(f"  FINDING: {b.label} decodes {bd / ad:.2f}x faster than {a.label}.")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", default=DEFAULT_BASE, help="Ghostlink base URL")
    ap.add_argument("--runs", type=int, default=3, help="measured runs per configuration (warmup excluded)")
    ap.add_argument("--prompt-tokens", type=int, default=1200, help="approximate prompt size in words")
    ap.add_argument("--label", default="run", help="label for this configuration")
    ap.add_argument("--compare", default="", help="comma-separated labels to compare, e.g. local,rpc")
    ap.add_argument("--no-warmup", action="store_true", help="skip the warmup request")
    ap.add_argument(
        "--overhead-only",
        action="store_true",
        help="measure only per-request fixed overhead (a five-word prompt) and exit",
    )
    args = ap.parse_args()

    try:
        token = open(TOKEN_PATH, encoding="utf-8").read().strip()
    except OSError:
        print(f"error: cannot read API key from {TOKEN_PATH}", file=sys.stderr)
        return 2

    bench = Bench(args.base, token)
    print(f"waiting for {args.base} ...", flush=True)
    if not bench.wait_ready():
        print("error: server did not become healthy", file=sys.stderr)
        return 2

    ctx = bench.describe()
    print(f"measuring: model={ctx.get('model')} mcp={len(ctx.get('mcp_servers') or [])} servers")

    if args.overhead_only:
        # The short-prompt probe, on its own. Exits here so it can be run in CI or a
        # pre-commit hook as a regression gate on prompt assembly, independently of any
        # throughput configuration.
        oh = measure_overhead(bench, runs=max(3, args.runs))
        print("\nper-request fixed overhead (five-word prompt)")
        print(f"  prompt tokens : {oh['prompt_tokens']}")
        print(f"  ttft          : {fmt(oh['ttft_ms'], ' ms', 12)}")
        if oh["errors"]:
            print(f"  errors        : {oh['errors']}")
        # A number, not a threshold: this tool reports, it does not gate. A hard limit
        # would need a per-model baseline, and a baseline that silently encodes today's
        # tool catalog is exactly the kind of number that rots.
        return 0

    res = bench.measure(
        args.label, args.runs, args.prompt_tokens, warmup=0 if args.no_warmup else 1
    )
    report([res], ctx)

    # Always show the overhead alongside throughput. The two answer different questions
    # and averaging them hides both: a healthy prefill rate says nothing about whether
    # every request is carrying thousands of tokens of constant.
    oh = measure_overhead(bench, runs=max(3, args.runs))
    print("\nper-request fixed overhead (five-word prompt)")
    print(f"  prompt tokens : {oh['prompt_tokens']}")
    print(f"  ttft          : {fmt(oh['ttft_ms'], ' ms', 12)}")

    if args.compare:
        names = [n.strip() for n in args.compare.split(",") if n.strip()]
        by_label = {res.label: res}
        if len(names) == 2 and all(n in by_label for n in names):
            compare(by_label[names[0]], by_label[names[1]])
        else:
            print("\n(comparison needs two measured labels; only one was run)")

    return 0


if __name__ == "__main__":
    sys.exit(main())