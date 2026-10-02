# Ghostlink Studio - Performance Benchmarks

> [!NOTE]
> **Current truth (Verified: 2026-08-18)**
> - **Proven:** Distributed VRAM/RAM capacity splitting across heterogeneous LAN nodes via `ggml-rpc` (e.g., loading and running a 30B-class MoE model split across nodes that individual machines cannot hold alone).
> - **Unproven / In Progress:** High usable tokens-per-second (tok/s) throughput on network-bound split layers, and a fully automated zero-touch "one-command cluster" setup without manual network/contributor configuration.


## Overview

This document contains performance benchmarks for the Ghostlink Studio system, measuring throughput and latency across different configurations.

**Note**: All benchmarks are measured with optimized in-memory transport. AF_XDP kernel-bypass is not currently implemented; see `README.md` for details.

---

## Full-Spectrum Session Benchmark — 2026-07-25 (laptop iGPU host)

Real, measured results from a single benchmarking session on the hardware below,
following the repo's own validated command set (`TESTING.md`). Reported here
because this host's profile (modest integrated GPU, Windows, native dev
launch) differs sharply from the RTX 4090 desktop numbers earlier in this
document — both are legitimate, they're just different machines.

### Hardware

| | |
|---|---|
| CPU | 16 logical cores |
| System RAM | 27.6 GB |
| GPU | AMD Radeon(TM) 860M (integrated) |
| GPU backend | DirectML, 4.0 GB VRAM |
| OS | Windows 11 |
| Build | `cargo build --release` — workspace `lto = "thin"`, `codegen-units = 1` (already the repo default) |

Detected via `ghost-link.exe probe local-node` — fast and full probe modes
agreed, so hardware detection needed no fixes this round.

### Methodology note: this host is noisy

Two back-to-back `cargo bench -p ghostlink-core --bench criterion` runs, **zero
code changes between them**, showed 5-80% swings on most benchmarks and a
one-off 3-4x hit on the multi-threaded ring buffer test. A 12-round
interleaved A/B (below) put steady-state flow throughput stdev at ~15-19% of
the mean. Treat single-run deltas on this machine with real skepticism —
absolute numbers and multi-run distributions are the trustworthy signal, not
one-shot "improved/regressed" percentages.

### Primitives (Criterion, `cargo bench -p ghostlink-core --bench criterion`)

Absolute timings from a clean run (background services stopped):

| Benchmark | Time |
|---|---|
| `ring/push_pop_round_trip/st` | 1.13 ns |
| `ring/push_only/st` | 2.6-2.7 ns |
| `ring/spsc_throughput/mt` | 7-11 ns (noisiest primitive — thread-scheduling sensitive) |
| `protocol/encode` | 61.2-61.8 ns |
| `protocol/decode` | 90-95 ns |
| `protocol/round_trip` | 154-224 ns |
| `planning/33_layers_2_nodes` | 89-112 ns |
| `planning/80_layers_8_nodes` | 117-140 ns |
| `planning/80_layers_8_nodes_autotuned` | 283-292 ns |
| `cluster/register_update` | 161-183 ns |
| `cluster/nodes_snapshot_10` | 18-20 ns |
| `cluster/calculate_cluster_health_10` | 15-16.5 ns |
| `autotune/detect_runtime_profile_fast` | 149-256 ns |
| `autotune/detect_runtime_profile_full` | 1.38 s (real hardware/WMI probe — startup-only cost, not a hot path) |
| `autotune/load_balance_80_layers_autotuned` | 1.23-1.31 µs |
| `autotune/accelerator_scale_f32_slice` | 577-641 ns |
| `fabric_inmem_single_gpu` | 322 µs |
| `fabric_tcp_two_stage_split` | 909 µs |
| `fabric_tcp_micro_batch_latency` | 1.16 ms |
| `fabric_tcp_four_stage_split` | 70.5 ms |

Full artifact: `python3 scripts/summarize_criterion_report.py --criterion-root target/criterion --output artifacts/criterion-summary.json`.

### Full pipeline (`scripts/flow_perf_snapshot.py`, release build)

`docs/PERF_BASELINE.json` and `docs/PERF_BASELINE_STRESS.json` were both
captured at `exec_tokens=512 micro_batch=8` — matching that (rather than the
script's own bare-invocation default of 256/4, which compares a different
workload) is required for a meaningful drift check:

| Mode | Throughput (avg) | P95 | vs. baseline |
|---|---|---|---|
| tcp (matched to `PERF_BASELINE.json`) | 228,626 tok/s | 2.20 ms | -10.7% (baseline 256,020) — within 45% drop tolerance |
| inmem (matched to `PERF_BASELINE.json`) | 346,932 tok/s | 1.39 ms | -31.5% (baseline 506,809) — within 40% drop tolerance |
| tcp (stress profile, 12 runs) | 234,983 tok/s | 2.14 ms | passed vs. `PERF_BASELINE_STRESS.json` |
| inmem (stress profile, 12 runs) | 403,635 tok/s | 1.21 ms | passed vs. `PERF_BASELINE_STRESS.json` |

`check_perf_drift.py`, `validate_stage_tail_metrics.py`, `validate_flow_canary.py`,
and `validate_flow_metrics_schema_contract.py` all passed on this run. Net
read: this laptop iGPU is meaningfully slower than whichever machine set the
baseline, but well inside the repo's drift tolerances — no regression.

Per-stage percentile analysis (`analyze_flow_stage_metrics.py`) showed
`avg_recv_wait_ms` growing ~20-25x from stage 0 (0.0008 ms) to stage 8-10
(0.017-0.02 ms) — expected pipeline-depth backpressure accumulation across 11
sequential stages, not a bug, but relevant if tuning stage count for
lower-latency topologies.

### TCP autotune

`GHOSTLINK_TCP_AUTOTUNE=1` selected `max_inflight=64` over the default 256.
An initial 2-sample head-to-head suggested the default was ~20-30% faster; a
proper 12-round **interleaved** A/B (to control for time-based drift)
contradicted that:

| Config | Mean | Stdev |
|---|---|---|
| `max_inflight=64` | 238,393 tok/s | 36,629 (15%) |
| `max_inflight=256` | 252,035 tok/s | 47,642 (19%) |

The two distributions overlap almost completely — not a statistically
reliable difference on this hardware. Lesson for future runs on this class of
machine: don't trust a 2-3 sample comparison here, and note that
`GHOSTLINK_TCP_AUTOTUNE_TOKENS` defaults to 64 tokens regardless of your real
`execution_tokens`, so pass `GHOSTLINK_TCP_AUTOTUNE_TOKENS`/`_MICRO_BATCH`
matching your target workload if you want the sweep to optimize for it.

### llama-server flags (`docs/LOCAL_INFERENCE_TUNING.md`)

Tested default vs. this host's documented 4GB-VRAM-tier recommendation
(`-fa on -b 512 -ub 256 -ctk q8_0 -ctv q8_0`) on `Llama-3.2-1B-Instruct-IQ3_M`:

| Metric | Default | Tuned | Delta |
|---|---|---|---|
| Decode (200 tok, short prompt) | 75.0 tok/s avg | 75.0 tok/s avg | ~0% (expected — decode of a single stream doesn't benefit from batch/flash-attn tuning) |
| Prefill (1381-token prompt, cold) | 1,043.5 tok/s | 1,169.7 tok/s | +12% (expected direction — matches `LOCAL_INFERENCE_TUNING.md`) |

### Cross-model chat throughput — 2026-08-03 follow-up, same host

Real generation numbers across three locally-downloaded models, prompted by a
live report of ~2-minute chat timeouts on `Qwen3.5-9B-Q4_K_M`. Same prompt
("List ten uses for a smartphone, one per line.", 11-token prompt), same
`n_predict=150`, one discarded warm-up completion per model first (avoids
first-call Vulkan shader-compile skew) — single run per model, not the
multi-round averages the rest of this doc uses, so treat deltas here with the
same skepticism the noisy-host section above asks for.

| Model | Size on disk | Quant | Load time | Prefill | Decode |
|---|---|---|---|---|---|
| `Qwen3.5-9B-Q4_K_M` | 5.6 GB | Q4_K_M | 14.1 s | ~6.0 tok/s | ~11.2 tok/s |
| `Meta-Llama-3-8B-Instruct-IQ3_M` | 3.8 GB | IQ3_M | 9.9 s | 43.4 tok/s | 12.8 tok/s |
| `Llama-3.2-1B-Instruct-IQ3_M` | 657 MB | IQ3_M | 4.3 s | 375.3 tok/s | 74.8 tok/s |

Confirmed this is genuine GPU work, not a silent CPU fallback:
`Get-Counter '\GPU Engine(*)\Utilization Percentage'` showed the
`llama-server` process's `engtype_compute` instance at ~29% during the
`Qwen3.5-9B` generation.

Caveat: this compares model *and* architecture together (Qwen3.5 vs Llama-3),
not quantization in isolation — no Q3_K build of `Qwen3.5-9B` itself was on
disk to test, so it's not a controlled same-model comparison. Still, at a
similar parameter count (8B vs 9B) the more-compressed IQ3_M build's 7x
prefill advantage over Q4_K_M is a large enough gap that quantization is
clearly doing real work here, on top of whatever the architecture difference
contributes.

### What wasn't run

- **Flamegraph**: `cargo-flamegraph` 0.6.13 is installed, but Windows profiling
  goes through `blondie` (ETW), which requires an elevated terminal —
  `NotAnAdmin` from this session's shell. Also, the `flow` command itself
  completes in ~1-2 ms per invocation, too short for meaningful sampling
  without wrapping it in a loop or profiling the Criterion binary instead
  (which runs long enough). To profile later: open an Administrator terminal
  and run `flamegraph -o out.svg -- target\release\deps\criterion-<hash>.exe --bench`.
- AF_XDP kernel-bypass — not implemented on Windows, out of scope here.


## Full-Spectrum Session Benchmark — 2026-08-03 (Linux mini PC, no dedicated GPU)

Real, measured results from a genuinely different hardware class than
every other entry in this document — a low-power consumer mini PC with no
dedicated GPU, running native Linux rather than Windows. This is the real
Linux data point [ENTERPRISE_PLAN.md](archive/ENTERPRISE_PLAN.md)'s Track A flagged
as missing (this is also the same host used for the real two-machine LAN
benchmark in the Multi-Node Performance section below).

### Hardware

| | |
|---|---|
| Host | `Desk-Mini` |
| CPU | 4 logical cores (Intel Alder Lake-N) |
| System RAM | 14.9 GB |
| GPU | Alder Lake-N UHD Graphics (integrated), Vulkan, 0.0 GB dedicated VRAM reported |
| OS | Linux |
| Build | `cargo build --release` (workspace default `lto = "thin"`, `codegen-units = 1`) |

Detected via `ghost-link probe local-node` (same command used for the
Multi-Node Performance entry below).

### Methodology note: contention explains the primitives, but not the full pipeline

The first `cargo bench`/`flow_perf_snapshot.py` run on this host ran while
`launch.sh`'s own servers (API, control-plane, llama-server, Vite dev
server) were still up, competing for all 4 cores. That run showed Criterion
reporting "Performance has regressed +20-40%" on nearly every primitive
relative to this host's previously-stored baseline — a real, expected
effect of CPU contention. After stopping every Ghostlink process and
re-running clean, most primitives improved as expected (raw logs below).

The full-pipeline `flow_perf_snapshot.py` numbers did **not** follow that
pattern — the "clean" run's average throughput was *lower* than the
contended run's (tcp: 76.5k clean vs. 114.4k contended tok/s; inmem: 134.4k
clean vs. 178.2k contended tok/s), and both runs show an enormous
run-to-run spread within their own 5 samples (clean inmem alone:
77.5k-187.4k tok/s, a 2.4x range). On this specific low-power 4-core host,
5 runs isn't enough for a stable mean, and other confounds (thermal
throttling on an N-series chip, memory-bandwidth sharing with the
integrated GPU) likely dominate over whatever contention stopping the
app's own servers removed. Treat every number below as directional for "a
weak consumer-grade mini PC," not a tight estimate — this host is noisier
than the AMD laptop profile above, which was already flagged as noisy.

### Primitives (Criterion, clean run — background services stopped)

| Benchmark | Time |
|---|---|
| `ring/push_pop_round_trip/st` | 3.15-3.22 ns |
| `ring/push_only/st` | 5.07-5.15 ns |
| `ring/spsc_throughput/mt` | 12.15-12.83 ns |
| `protocol/encode` | 85.1-86.3 ns |
| `protocol/decode` | 101.4-104.0 ns |
| `protocol/round_trip` | 204.0-212.6 ns |
| `planning/33_layers_2_nodes` | 140.1-141.0 ns |
| `planning/80_layers_8_nodes` | 334.2-342.4 ns |
| `planning/80_layers_8_nodes_autotuned` | 424.6-428.6 ns |
| `cluster/register_update` | 235.3-237.3 ns |
| `cluster/nodes_snapshot_10` | 30.8-31.4 ns |
| `cluster/total_vram_10` | 927-935 ps |
| `cluster/calculate_cluster_health_10` | 45.1-47.0 ns |
| `autotune/detect_runtime_profile_fast` | 106.9-108.8 ns |
| `autotune/detect_runtime_profile_full` | 19.18-20.48 ms |
| `autotune/load_balance_80_layers_autotuned` | 1.616-1.632 µs |
| `autotune/accelerator_scale_f32_slice` | 2.186-2.224 µs |

`fabric_*` benchmarks (present in the Windows laptop entry above) did not
appear in this run's output on this host — noted rather than guessed at;
worth checking whether that's a platform feature gate or a suite change
before assuming anything from their absence.

### Full pipeline (`scripts/flow_perf_snapshot.py`, `exec_tokens=512 micro_batch=8`, release build)

| Run | Mode | Throughput (avg) | Throughput (min-max) | P95 (avg) |
|---|---|---|---|---|
| Contended (app servers running) | tcp | 114,429.68 tok/s | 78,979.10-134,124.42 | 4.58 ms |
| Contended (app servers running) | inmem | 178,194.78 tok/s | 82,469.41-272,413.03 | 3.56 ms |
| Clean (servers stopped) | tcp | 76,455.51 tok/s | 63,534.12-89,957.73 | 6.78 ms |
| Clean (servers stopped) | inmem | 134,359.68 tok/s | 77,473.96-187,417.62 | 4.10 ms |

Compared against `docs/PERF_BASELINE.json` (256,020 tok/s tcp / 506,809
tok/s inmem, captured on different, more powerful hardware): this host's
clean-run numbers land roughly 70-73% below that baseline — far outside the
45%/40% drop tolerances defined there. That's not a regression; it's
confirmation that a 4-core integrated-graphics mini PC is a genuinely
different performance tier from whatever machine set that baseline. The
honest takeaway from this host isn't a specific throughput figure (the
spread above is too wide to trust any single number) — it's that Ghostlink
runs correctly end-to-end on real, low-power, no-dedicated-GPU Linux
hardware, which is itself a useful data point for the project's "runs on
whatever's already on your LAN" positioning.

Raw logs (on the Desk-Mini host itself, not committed to this repo):
`~/criterion_desk_mini.log` / `~/flow_perf_desk_mini.log` (contended run)
and `~/criterion_desk_mini_clean.log` / `~/flow_perf_desk_mini_clean.log`
(clean run).

## Other hardware profiles

Real, measured single-node numbers now exist in this document for three
hardware classes: an AMD integrated GPU laptop (Windows), an Intel
i7-14700K (Linux/WSL2, see the README's own
[Performance section](../README.md#performance)), and a low-power Linux
mini PC with no dedicated GPU (above). This repo previously carried
fabricated placeholder tables here for CPU/GPU configurations (8-core
Intel/AMD, RTX 4090) and model throughput that were never actually run —
they've been removed rather than left as bait for anyone skimming this
doc. Still missing: a discrete NVIDIA/AMD GPU, Apple Silicon, and a
server-class CPU. If you benchmark Ghostlink on one of those,
`scripts/flow_perf_snapshot.py` and
`cargo bench -p ghostlink-core --bench criterion` are the repo's real,
runnable benchmark tools (see the Full-Spectrum sessions above for exact
invocations and output format) — a PR adding a dated, methodology-labeled
entry for a new hardware class is a genuinely useful contribution.

---

## Multi-Node Performance

### Real two-machine LAN run — 2026-08-03

The first real entry for this section, replacing the fabricated 2/3/4-node
placeholder table this repo used to carry (see git history) — that table
predated `stage-worker`/`flow --remote-addr` even existing, so no version
of Ghost-Link could have produced it.

**Hardware — two genuinely separate machines on the same residential LAN:**

| | Coordinator (local) | Remote (`stage-worker`) |
|---|---|---|
| Host | Windows 11 laptop | `Desk-Mini` (Linux) |
| CPU | 16 logical cores | 4 logical cores (Intel Alder Lake-N) |
| System RAM | 27.6 GB | 14.9 GB |
| GPU | AMD Radeon 860M (integrated), 4.0 GB VRAM, DirectML | Alder Lake-N UHD Graphics (integrated), Vulkan, 0.0 GB dedicated VRAM reported |
| Network | Wi-Fi | unknown (not confirmed Ethernet vs. Wi-Fi) |

ICMP round-trip between the two over this LAN measured 8-14ms
(`ping Desk-Mini`), consistent with the TCP bridge-write latency below.

**A real capacity-modeling caveat, stated plainly**: `print_flow` (the code
behind the `flow` subcommand this harness drives) hardcodes the local
node's declared capacity to a minimum of 16GB
(`crates/ghost-link/src/main.rs`, `print_flow`, `.max(16.0)`) for its fixed
60-layer/30GB synthetic scenario — that's a demo-scenario convenience, not
a claim about this laptop's real ~4GB VRAM. Desk-Mini's real probed
capacity (`ghost-link probe local-node`) is 0.0 GB dedicated VRAM / 14.9 GB
system RAM — an integrated-GPU mini PC that could not actually hold a
meaningful share of a real 30B-class model's weights. Passing that real
number to `--remote-vram-gb` produces a degenerate all-or-nothing
placement (whichever side has more declared capacity absorbs the entire
synthetic workload, leaving the other with zero assigned stages — this
also happened during setup with the *opposite* value: `--remote-vram-gb
32` handed all 60 layers to Desk-Mini and none to the coordinator, which
then hard-errored). `--remote-vram-gb 16` was used here as a **declared
test input to force a genuine two-node split**, not as a claim about
Desk-Mini's real capability. This is the same limitation
[DEPLOYMENT.md's Stage 3b](DEPLOYMENT.md#stage-3b-real-cross-machine-flow-execution)
already documents: per-stage compute is a synthetic timing proxy, not real
model math — what this benchmark actually validates is the **real
cross-machine transport** (genuine `TcpStream::connect`, real framed
batches, real measured round-trip latency), not real distributed LLM
throughput.

```bash
python scripts/remote_flow_benchmark.py --remote-addr <desk-mini-ip>:9500 \
  --runs 5 --release --remote-vram-gb 16 --remote-mem-gb 14.9
```

(worker side, restarted fresh before each one-shot run:
`GHOSTLINK_TCP_AUTH_TOKEN=local-token ghost-link stage-worker 0.0.0.0:9500`)

| Runs | Throughput (avg) | Throughput (min-max) | P95 (avg) | Remote bridge-write (avg) | Remote bridge-write (min-max) |
|---|---|---|---|---|---|
| 5 | 337.4 tok/s | 253.4-385.2 tok/s | 19.50 ms | 12.11 ms | 10.37-15.77 ms |

Every run's `stage_stats` confirms 2 pipeline stages — stage 0 (local,
compute-only, ~0.008ms, zero bridge-write) and stage 1 (Desk-Mini, zero
compute, 10.4-15.8ms bridge-write) — meaning every run genuinely crossed
the real network for its remote stage, not a loopback shortcut. Raw output:
`tmp/remote_flow_benchmark/` (`summary.json` + one `remote-N.json` per run).

The absolute throughput number here (`tokens/sec` of the synthetic pipeline
harness) isn't comparable to the Full-Spectrum session's in-process numbers
above — different code path, different hardware pair, and a much lower
`exec_tokens`/`micro_batch` (this harness's defaults: 256/4). The real
signal is `remote_bridge_write_ms`: a genuine ~10-16ms round-trip write
cost to a physically separate machine over Wi-Fi, in the same range as raw
ICMP RTT to that host.

### Real ggml-rpc distributed-inference run — 2026-08-08 (Docker fabric, CPU-only, single host)

The 2026-08-03 entry above is explicit that it exercises the *synthetic*
`stage-worker`/`flow` pipeline harness (fake `f32` payloads, a timing proxy
for per-stage compute), not Ghostlink's real distributed-inference feature.
This entry supersedes/complements it by exercising the actual production
code path: llama.cpp's own `ggml-rpc` backend
(`crates/ghost-link/src/rpc_cluster.rs`), via `docker-compose.rpc-fabric.yml`
(a contributor container + a coordinator container on an isolated Docker
bridge network) and its CI correctness gate,
`.github/workflows/distributed-e2e.yml` /
`scripts/rpc_fabric_assert.py` — the same evidence bar that gate uses
(`real_inference: true` in the chat response, plus fresh "Accepted client
connection" lines in the contributor's own `ggml-rpc-server` log) is what
every run below is checked against, not just a settings flag.

**Important framing**: both "nodes" here are containers on **one physical
host**, not genuinely separate machines like the 2026-08-03 LAN entry above
(real `stage-worker` on a second machine, real NIC hop). This entry can
prove the real cross-process ggml-rpc code path works and measure its real
local resource cost — it cannot show a real network-bandwidth or
genuinely-separate-hardware benefit, and the results below should not be
read as such.

#### Hardware / resource limits

| | |
|---|---|
| Host OS | Windows 11 (build 10.0.26200) |
| Docker Desktop VM | 16 CPUs, 13.46 GiB RAM (`docker info --format 'NCPU={{.NCPU}} MemTotal={{.MemTotal}}'` → `NCPU=16 MemTotal=14451666944`), Linux 6.6.87.2-microsoft-standard-WSL2, Docker 29.6.2 |
| GPU | None passed through — CPU-only for both containers, `-ngl` forced per below |
| `rpc-bench-contributor` limit | `cpus: '4'`, `mem_limit: 3g` |
| `rpc-bench-coordinator` limit | `cpus: '4'`, `mem_limit: 3g` |

(Larger than the CI gate's `2 cpus`/`2g` per container — this benchmark's
model is ~59x the CI gate's 19MB smoke-test model, and needed real headroom
to run repeated timed trials rather than a single pass/fail check.)

#### Model

`Qwen2.5-1.5B-Instruct-Q4_K_M.gguf`, downloaded directly from Qwen's own
official GGUF repo on Hugging Face (public, unauthenticated,
`https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf`
— same plain-HTTPS-GGUF-URL convention `Dockerfile.rpc-fabric` already uses
for `stories15M-q4_0.gguf`). 1.5B parameters, Q4_K_M quantization, real
downloaded file size **1,117,320,736 bytes (1.04 GiB)**,
sha256 `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`.
Chosen as a real step up from the trivial 19MB stories15M smoke-test model
(which proves connectivity, not realistic throughput or memory behavior)
while still being CPU-feasible for several timed trials on this hardware in
a reasonable amount of time.

#### Methodology

New `scripts/rpc_fabric_benchmark.py` and `docker-compose.rpc-fabric-benchmark.yml`
(the latter is a **separate** compose file — `docker-compose.rpc-fabric.yml`
itself, the CI gate, is untouched). The benchmark model is bind-mounted
into the existing image's `/app/models` directory rather than baked into
`Dockerfile.rpc-fabric`, so the CI-gate image build is unaffected.

For each of the two phases below, the script: PATCHes `distributed_inference`
live via `/api/settings`, then calls `/api/models/load` — which always
fully restarts `llama-server` (`native_engine.rs::load_model_into_slot`'s
own doc comment: "restarting it with the new model... doesn't support
runtime hot-swapping"), so the restart genuinely picks up the new
`--rpc`/`-ts` flags — then runs **5** real chat completions
(`POST /api/inference/chat`, which — unlike `/v1/chat/completions` — reports
real per-call `tokens_generated` and server-measured `throughput`/`latency_ms`)
against a fixed prompt, `max_tokens=128`. Every distributed-phase run is
checked for `real_inference: true` *and* a nonzero increase in the
contributor's `ggml-rpc-server` "Accepted client connection" log count
since the phase started — a passing response alone doesn't prove real
distributed work happened, same standard `rpc_fabric_assert.py` holds
itself to. Memory is read directly from each container's cgroup
(`/sys/fs/cgroup/memory.current` + `memory.stat`'s `anon`/`file` split),
not `docker stats`' derived/cache-inclusive number — the anon/file
distinction turned out to be the load-bearing one (see below).

#### Result 1 — as-shipped default (`-ngl 0`, the same CPU-safety default `scripts/rpc_fabric_entrypoint.sh` forces for every container in this fabric, including the CI gate)

| Mode | Runs | Throughput (avg) | Throughput (min-max) | Latency (avg) | Coordinator mem post-load | Coordinator anon/file split |
|---|---|---|---|---|---|---|
| single-node (`distributed_inference: false`) | 5 | 32.70 tok/s | 32.48-32.96 tok/s | 3754.2 ms | 1,940,283,392 B (1.807 GiB) | anon 811,155,456 B (773.5 MiB) / file 1,118,490,624 B (1.042 GiB) |
| distributed (`distributed_inference: true`) | 5 | 32.53 tok/s | 32.40-32.70 tok/s | 3776.3 ms | 1,940,000,768 B (1.807 GiB) | anon 811,634,688 B (774.0 MiB) / file 1,118,543,872 B (1.042 GiB) |

Throughput and memory are statistically indistinguishable between the two
modes (0.5% throughput delta, well inside the ~0.1-0.2 tok/s run-to-run
stdev measured within each mode). **But the distributed run is not a
no-op**: the contributor's `ggml-rpc-server` log genuinely gained **462**
new "Accepted client connection" lines over the 5 runs (0 in single-node,
where `distributed_inference` was off) — real RPC traffic occurred. The
explanation, found by reading `native_engine.rs::build_cmd`'s actual
launched command line
(`... -ngl 0 ... --rpc 172.31.0.11:50052 -ts 0.1000,0.1000`): with `-ngl 0`,
llama.cpp assigns **zero** transformer layers to any non-CPU-primary
backend device — GPU or RPC alike — so `--rpc`/`-ts` are real flags
producing a real (but layer-empty) connection, not real tensor placement.
`-ngl 0` is what `scripts/rpc_fabric_entrypoint.sh` forces by default for
every container in this fabric (`export GHOSTLINK_LLAMA_NGL="${GHOSTLINK_LLAMA_NGL:-0}"`,
a deliberate CPU-only safety default, not a bug) — meaning **the CI gate's
own default configuration exercises the RPC wire protocol for real, but
not real cross-process layer compute.**

#### Result 2 — control experiment: real layer offload (`GHOSTLINK_BENCH_NGL=-1`)

To confirm that diagnosis rather than assume it, the coordinator was
recreated with `-ngl -1` (`native_engine.rs::get_ngl`'s own "auto/offload
all" fallback) and 2 more real distributed runs collected
(`max_tokens=96`; 2 runs rather than 5 — this was a confirmatory control,
not the primary comparison, so it gets less statistical weight):

| | as-shipped (`-ngl 0`, distributed) | control (`-ngl -1`, distributed) |
|---|---|---|
| Coordinator mem post-load | 1,940,000,768 B (1.807 GiB) | **1,195,409,408 B (1.113 GiB)** |
| Coordinator anon (real committed memory) | 811,634,688 B (774.0 MiB) | **70,815,744 B (67.5 MiB)** |
| Coordinator file (mmap'd GGUF, reclaimable cache) | 1,118,543,872 B (1.042 GiB) | 1,117,343,744 B (1.040 GiB) |
| Contributor mem post-load | 7,139,328 B (6.8 MiB) | **1,129,553,920 B (1.052 GiB)** |
| Contributor anon | 3,457,024 B (3.3 MiB) | **1,123,610,624 B (1.046 GiB)** |
| New contributor RPC connections | +462 over 5 runs (~92/run) | +1084 over 2 runs (~542/run) |
| Throughput (avg) | 32.53 tok/s | **17.01 tok/s** (16.86-17.17, 2 runs) |
| Latency (avg) | 3776.3 ms | 5047.8 ms |

This is the real answer to "does ggml-rpc's tensor split reduce the
coordinator's own local memory footprint": **yes, on the metric that
actually matters for avoiding an OOM — real anonymous (committed) memory —
when layer offload is genuinely enabled.** The coordinator's anon memory
dropped **~741 MB** (811.6 → 70.8 MiB) while the contributor's anon memory
grew by almost exactly that much (3.3 → 1046 MiB), consistent with roughly
half the model's real working weight buffers moving to the remote side
under the `-ts 0.1000,0.1000` split (both nodes report equal declared VRAM
— 0.0 GB, CPU-only — so `rpc_cluster::compute_tensor_split` floors both to
an even split; these two containers also happened to have equal RAM, so the
even split was correct here. When an all-CPU-only cluster's nodes have
*unequal* RAM, `compute_tensor_split` computes weighted capacity: discrete VRAM is used for GPU layers, while 0 VRAM CPU-only nodes use system RAM scaled by a conservative 0.5 haircut (`CPU_RAM_HAIRCUT`).

**But it is not a proportional total-memory reduction**, and this is the
honest, load-bearing caveat: the coordinator's `file` (mmap'd GGUF page
cache) stayed at ~1.04 GiB — essentially the entire model file — in
**both** configurations, single-node or distributed, `-ngl 0` or `-ngl -1`.
`native_engine.rs::build_cmd` always passes `-m <full local model path>`
and never passes `--no-mmap`; llama.cpp mmaps the whole GGUF locally
regardless of how much of it actually executes remotely. Those mmap'd
pages are reclaimable clean file-backed cache (the kernel can drop and
re-read them under real memory pressure, unlike the anon pages above), but
they still count toward `memory.current` and toward what a cgroup
`mem_limit` would eventually reclaim-under-pressure or OOM-kill over,
depending on how tight the limit is and how the reclaim path behaves —
this was not stress-tested at a limit tight enough to force that decision
(both configurations ran comfortably inside the 3g `mem_limit`, nowhere
near triggering reclaim or OOM), so this document does **not** claim to
have shown a "model too large for one node alone, only fits when split"
capacity unlock. The honest, measured claim is narrower and still real:
tensor-split genuinely redistributes committed working memory when layer
offload is actually enabled, but the coordinator's local address space
still has to mmap the full file either way.

**And real distributed compute has a real cost here, not a benefit**:
throughput dropped ~48% (32.53 → 17.01 tok/s) once compute was genuinely
split across the RPC link. This is expected, not a regression to chase —
both "nodes" are containers time-sharing **one physical host's CPU cores**
with no additional hardware added by distributing, so every real
cross-process tensor op pays real IPC/RPC round-trip and serialization
overhead (~542 accepted connections per single 96-token generation call,
vs. ~92/call when the split was compute-inert) for zero added compute
capacity. This matches the same-host-no-real-benefit caveat the
2026-08-03 LAN section above states from the transport side; here it's
measured from the compute-distribution side instead.

#### Caveats

- CPU-only throughout — no GPU passthrough in this Docker setup. Not a
  GPU-cluster performance claim.
- Both containers ran on one physical host (Docker Desktop VM), not two
  genuinely separate machines — unlike the 2026-08-03 LAN entry, this
  cannot and does not measure real network bandwidth/latency between
  separate hardware.
- `tokens_estimated` (prompt-side count) is a word-count estimate
  (`req.message.split_whitespace().count()`in `handle_gui_chat`), not an
  exact tokenizer count — not relied on for the throughput numbers above,
  which use the server's real `tokens_generated`/measured-latency
  `throughput`.
- The first run in each 5-run series generated fewer tokens (98 and 106 of
  a requested 128) than the rest — the model hit a natural stop condition
  early on that particular generation; throughput (tokens ÷ latency)
  already accounts for this, but it's why per-run token counts in the raw
  JSON aren't all identical.
- The `-ngl -1` control used 2 runs, not 5, and a shorter `max_tokens` (96
  vs. 128) — a real, deliberate, disclosed asymmetry with the primary
  table above, not an oversight; it was a confirmatory experiment run
  after the primary comparison, not the main result.
- Only one model size/quantization was tested. Whether the anon-memory
  split holds proportionally for larger models, or whether a genuine
  OOM-vs-succeeds capacity unlock is reachable at a large-enough model /
  tight-enough `mem_limit` combination, is real open follow-up work, not
  something this entry measured.

#### Reproduce this section

```bash
# One-time: download the benchmark model (public, unauthenticated HF repo)
curl -L --fail -o models-bench/qwen2.5-1.5b-instruct-q4_k_m.gguf \
  https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf

docker compose -f docker-compose.rpc-fabric-benchmark.yml up -d --build --wait --wait-timeout 300
python3 scripts/rpc_fabric_benchmark.py --runs 5 --max-tokens 128

# Control experiment: real layer offload instead of the CPU-safe -ngl 0 default
GHOSTLINK_BENCH_NGL=-1 docker compose -f docker-compose.rpc-fabric-benchmark.yml up -d rpc-bench-coordinator --wait
python3 scripts/rpc_fabric_benchmark.py --skip-single-node --runs 2 --max-tokens 96 \
  --output-dir tmp/rpc_fabric_benchmark_ngl_control

docker compose -f docker-compose.rpc-fabric-benchmark.yml down -v
```

Raw per-run output: `tmp/rpc_fabric_benchmark/` (`summary.json` +
`single-node-N.json`/`distributed-N.json`) and
`tmp/rpc_fabric_benchmark_ngl_control/` — not committed (`tmp/` is
gitignored), same convention as the 2026-08-03 LAN entry's raw output
above.

### Real ggml-rpc distributed-inference run — 2026-08-08 (native processes, two genuinely separate physical machines)

The entry above proved the real `ggml-rpc` mechanism works but explicitly
could not measure a genuine separate-hardware benefit, because both
"nodes" were containers sharing one physical host's CPU. This entry closes
that gap — real `ghost-link serve` processes (no Docker) on two actually
separate machines on the same residential LAN — and in the process found
four real, distinct bugs that the single-host Docker test structurally
could not have surfaced. This is the most important entry in this
document: it's the first genuinely separate-hardware run of the real
(non-synthetic) distributed-inference path, and it found real correctness
and resilience problems, not just a throughput number.

**Hardware — two genuinely separate machines on the same residential LAN:**

| | Coordinator (`10.0.0.87`) | Contributor "Iprada" (`10.0.0.29`) |
|---|---|---|
| OS | Windows 11 | Linux |
| GPU / compute exposed to RPC | AMD Radeon(TM) 860M (integrated), Vulkan, 4.0 GB VRAM | Intel(R) N97 CPU only (15.2 GiB free) — the machine also has an Alder Lake-N iGPU, but only its CPU was exposed as an RPC device in this test |
| Declared VRAM (drives tensor-split ratio) | 3.999 GB | 0.0 GB (floored to 0.1 GB by `compute_tensor_split`) |
| Role | `distributed_inference: true` | `contribute_compute: true`, `rpc_port: 50052` |

ICMP round-trip measured 8-14ms, consistent with the 2026-08-03 LAN
entry's separate hardware pair (10-16ms) — a genuine second real-network
data point, not a coincidence to read too much into with N=1 pairs.

Both processes ran natively (`cargo build --release`, llama.cpp built with
`-DGGML_RPC=ON` via the same cmake recipe as `Dockerfile.rpc-fabric`),
deliberately avoiding Docker Desktop's Windows/WSL2 networking layer for
this test — real UDP broadcast discovery (`GET /api/workers/discover` →
`count: 2`) confirmed this works cleanly between genuinely separate
physical machines with no manual IP configuration beyond the standard
LAN.

#### Result 1 — 1.5B model (`Qwen2.5-1.5B-Instruct-Q4_K_M`, the same file as the Docker entry above): real, small, honest cost

`--rpc 10.0.0.29:50052 -ts 3.9990,0.1000` — built entirely by Ghostlink
itself from live discovery, no manual flags. 5 runs each, `POST
/api/inference/chat`, same prompt/`max_tokens=128` methodology as the
Docker entry:

| Mode | Runs | Throughput (avg) | Throughput (min-max) | Stdev |
|---|---|---|---|---|
| single-node | 5 | 55.11 tok/s | 54.27-56.23 tok/s | 0.70 |
| distributed | 5 | 53.57 tok/s | 53.32-53.95 tok/s | 0.25 |

A real ~2.8% cost — far smaller than the Docker same-host entry's 48%
penalty, because real separate hardware means the coordinator isn't
time-sharing its CPU with the remote side. But it's a cost, not a benefit,
for a structural reason: `compute_tensor_split` is purely
VRAM-proportional (`local_vram_gb.max(0.1)` per node — see
`rpc_cluster.rs`), and Iprada declares `0.0 GB` VRAM (its iGPU wasn't
exposed as an RPC device, only its CPU, which the split formula has no way
to value in GB terms). That floors Iprada to a token ~2.4% share
regardless of how much real, usable CPU/RAM capacity it actually has —
real "old laptop with plenty of system RAM, no dedicated VRAM" hardware,
exactly the profile the product pitch is built around, and the current
split heuristic can't make meaningful use of it. **Worth carrying into any
automatic-sharding work**: a VRAM-only split heuristic systematically
under-uses non-GPU compute contributors.

#### Bug 1 — quantized KV cache crashes the remote RPC-CPU backend (found, and already silently handled)

The very first model load on this pairing crashed on both sides at once:
the coordinator's own log recorded `llama-server exited before becoming
ready (status: exit code: 0xc0000409)`, and Iprada's `ggml-rpc-server` log
(via the opt-in `GHOSTLINK_RPC_SERVER_LOG`, same mechanism the CI gate
uses) captured the other half of the same failure — a real GDB backtrace
ending in `ggml_get_n_tasks: op not implemented: 100` inside
`rpc_server::graph_compute`. Root cause: `-ctk q8_0 -ctv q8_0` (quantized
KV cache, `native_engine.rs`'s default for declared-VRAM-under-4GB tiers)
uses a ggml op the RPC/CPU backend combination doesn't implement.
`native_engine.rs` already has a real, working fallback — retry without
quantized KV cache — which is why every chat completion in Result 1 above
still succeeded; the crash and recovery happened silently during the very
first load and wasn't visible without reading the raw log.

**A second, separate bug this surfaced**: after that crash, Iprada's `ghost-link
serve` process stayed healthy (`/health` kept responding), but
`ggml-rpc-server` — the actual contributor child process
`rpc_cluster::ensure_contributing()` spawns once at startup — was dead
(port 50052 went from accepting connections to `Connection refused`).
Nothing in Ghostlink restarts it, and nothing stops the node from
continuing to *advertise* `contribute_compute`/`rpc_port` via discovery
(that's driven by settings, not live child-process health). A coordinator
would discover this node as a usable peer, build a `--rpc` flag pointing
at a dead port, and fail — a real resilience gap, not just a KV-cache
edge case. Confirmed the API layer and the actual compute layer can
silently diverge in health.

#### Result 2 — bigger models (7B, then a different 5GB single-file model): a real, reproducible, silent correctness bug

To test whether distribution could show a genuine capacity benefit (not
just a cost), the same pairing was loaded with
`Qwen2.5-7B-Instruct-Q4_K_M` (4.36 GiB, real 2-shard GGUF from Qwen's
official HF repo, sha256-verified) — comfortably over the coordinator's
declared 4.0 GB VRAM.

- **The distributed load did not fail to fit — it just took longer than
  Ghostlink's timeout allows.** `native_engine.rs`'s model-ready
  health-check budget is a hardcoded 90 seconds (`wait_for_llama_server_ready(&url,
  90, &mut child)`, no env override), identical for single-node and
  distributed loads. Running the *exact* logged distributed command
  manually (bypassing Ghostlink's timeout) showed the model **did** load
  successfully — after **5 minutes 3 seconds**. `llama.cpp`'s own log
  explained part of the slowdown: `failed to fit params to free device
  memory: model_params::tensor_split already set by user, abort` — its
  automatic memory-fit optimization disables itself whenever an explicit
  `-ts` is passed, which Ghostlink always does. Single-node loading of the
  *same* file completed within the 90s budget and served correct,
  coherent real output (`real_inference: true`, 38.71 tok/s) — the model
  fit and ran fine locally on this UMA (unified-memory) integrated GPU,
  which can apparently overflow gracefully into system RAM past its
  "declared" 4.0 GB. **A real, actionable bug**: the 90s readiness budget
  doesn't account for the real extra time an RPC-distributed load takes,
  and currently aborts loads that would have succeeded.

- **Far more serious, found once the distributed load was allowed to
  finish**: the response was **reproducibly corrupted garbage** —
  `"The capital of France is"` → `"pérdida RencontreDBus并不是很 pérdida..."`
  — while the server reported `healthy`/HTTP 200 throughout, and
  throughput collapsed to 1.7 tok/s (vs 38.71 tok/s single-node, same
  file). Reproduced on a **second, unrelated single-file model**
  (`gemma-4-E4B-it-Q4_K_M.gguf`, 4.97 GiB, not sharded — ruling out
  multi-part GGUF handling as the cause) with the same garbage pattern
  (`"’’’’’’’’’’’’’’’’’’’’"`). The 1.5B model in Result 1 above worked
  correctly through the identical distributed path on the identical
  hardware pair, so this isn't a blanket "RPC is broken" finding — it's
  specific to larger models on this pairing, and the trigger wasn't fully
  characterized before the likely cause below was found and confirmed.

- **Root cause, confirmed by a controlled before/after test**: the two
  machines' `llama.cpp` builds were on different commits — coordinator at
  `da296d6` (2026-07-23), Iprada at `e920c523e` (2026-07-13), a 10-day gap
  in a very actively developed upstream project. `ggml-rpc-server` has no
  version-compatibility check between peers (the same upstream limitation
  already documented for its lack of authentication — see the Risks
  section of `ROADMAP.md`) — mismatched builds connect and exchange data
  without complaint instead of rejecting each other. After rebuilding
  Iprada's `llama.cpp` at the exact matching commit (`da296d6`, confirmed
  via the `ggml` version string bumping 0.16.0 → 0.17.0) and rerunning the
  identical `gemma-4-E4B-it-Q4_K_M` distributed load — same model, same
  tensor split, same real cross-machine RPC path, only the peer's
  `llama.cpp` commit changed — the output was **correct**: `"The capital
  of France is"` → `"Paris. Paris is a famous city known for its art,
  fashion, and history. The Eiffel Tower"`. Load time also dropped from
  312s to 168s on the matched build. This is a real, controlled A/B
  result, not circumstantial: **version-mismatched `ggml-rpc` peers can
  silently corrupt output for larger models while reporting healthy
  status throughout, and this is currently undetectable from the API
  surface alone.**

#### Result 3 — the actual proof: a real 30B-class model, genuinely too large for the coordinator alone, working correctly when split

With versions matched, the pairing was pushed with a real ~13.58 GiB
model already on hand locally: `Qwen3-Coder-30B-A3B-Instruct-Q3_K_L.gguf`
(30B-parameter MoE, ~3B active per token, Q3_K_L quant, sha256 not
re-verified since it predates this session — a pre-existing local file).

**Single-node genuinely cannot load this model** — not a timeout, a real,
clean failure:
```
ggml_vulkan: Device memory allocation of size 964611072 failed.
ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory
```
This is the first entry in this document where single-node isn't just
slow or suboptimal — it hard-fails. Real coordinator hardware (4.0 GB
declared VRAM, UMA integrated GPU) cannot hold this model alone.

**The distributed attempt also initially failed**, and the debugging
process (jointly diagnosed with the Claude session running on Iprada,
which had direct log access this session's coordinator didn't) is worth
recording because every intermediate hypothesis was real and ruled out in
turn, not assumed:

1. Three separate coordinator-side configurations — default settings,
   `-c 2048` (down from 8192, ruling out KV-cache size), and an extreme
   `-ts 39.9990,0.0100` split (~400x more lopsided than the automatic
   ratio, ruling out the split ratio controlling per-buffer size) — all
   failed identically: `failed to allocate RPC0[10.0.0.29:50052] buffer of
   size 1043927040`, same byte count every time. This looked like a fixed
   structural ceiling.
2. Checked Iprada's Vulkan `maxMemoryAllocationSize`: 11.15 GiB — far
   above the ~995 MB failing request, ruling out a hard per-allocation
   cap.
3. `GGML_RPC_DEBUG=1` on the contributor (upstream llama.cpp's own
   per-command debug logging, `ggml-rpc.cpp:20`) gave real command-level
   visibility and overturned the "fixed ceiling" read: the coordinator
   was actually streaming buffers successfully in sequence — **11
   buffers of ~947 MB–1.06 GB each, each properly `alloc_buffer` →
   `set_tensor` (real weight data, up to ~138 MB per call) →
   `free_buffer`, one per transformer layer — and only the 12th of the
   same size failed.** Summed, 11 successful buffers ≈ 10.5 GiB, right up
   against the 11.15 GiB heap. The real signature: not a per-allocation
   cap, and not a leftover-process leak (a freshly-restarted contributor
   failed identically) — **the Vulkan/Mesa driver on this iGPU wasn't
   fully reclaiming freed device memory across repeated large alloc→free
   cycles within one process**, so cumulative usage crept toward the heap
   ceiling regardless of individual buffer size or split ratio.
4. **The fix**: reduce `-ngl` from `-1` (offload all layers) to `20`,
   cutting the total number of layers assigned across local+remote
   devices — not the per-buffer size, the buffer *count*. This kept
   Iprada's cumulative allocation comfortably under the reclaim ceiling.

**Result: real success.** Loaded in 487s (~8 min — the largest model
tested this session, expected to take longer). Two real completions,
both coherent and correct — including a genuinely competent Python
Fibonacci implementation with edge-case discussion, real evidence this is
a working coding-capable model, not a lucky short answer:

```
"Write a Python function that returns the nth Fibonacci number."
→ "The Fibonacci sequence starts with F(0) = 0, F(1) = 1, and each
   subsequent number is the sum of the two preceding ones. ... 
   def fibonacci_nth(n):
       # Your implementation here
       pass
   def fibonacci_list(n): ..."
```

Real measured throughput: prompt eval 0.39–0.82 tok/s, generation
1.47–2.52 tok/s — slow, and an honest caveat this document should not
smooth over: at this speed, "usable" is a stretch for interactive chat.
This is **not yet** the "usable speed" half of the roadmap's target
claim, even though it is genuinely the "too large for one node, works
when split" half. Both halves together are the actual target; this
entry has one of them, on real hardware, for the first time.

##### Finding the real `-ngl` boundary on this pairing

`-ngl -1` (offload all layers) hits the reclaim ceiling above; `-ngl 20`
(Result 3's working config) doesn't. A manual bisection above `-ngl 20`
found the real boundary and a second, related failure mode:

| `-ngl` | Result | Load time | Prompt eval | Generation |
|---|---|---|---|---|
| 20 | works | 487s | 0.39–0.82 tok/s (2 runs) | 1.47–2.52 tok/s (2 runs) |
| 30 | works | 902s | 0.36 tok/s (1 run) | 1.56 tok/s (1 run) |
| 40 | **fails** | 11m41s of successful weight loading, then fails | n/a | n/a |

`-ngl 40`'s failure is instructive on its own: every weight buffer loaded
successfully this time (no `alloc_tensor_range` weight failures, unlike
Result 3's original `-ngl -1` failure) — it failed afterward, specifically
allocating the **KV cache**, at a much smaller size (~312 MB, not the
~995 MB weight-buffer chunks): `failed to allocate RPC0[...] buffer of
size 327155712` → `failed to allocate buffer for kv cache`. This confirms
the reclaim-ceiling bug isn't specific to weight-tensor transfers — it's
cumulative across *any* large allocation on the contributor's device
within one session, weights and KV cache competing for the same ~11 GiB
budget. At `-ngl 40`, enough weight data landed on Iprada that no room
was left for its share of the KV cache once inference was about to
start.

**Practical takeaway**: the real safe boundary for this exact
model/hardware/quant combination sits between `-ngl 30` and `-ngl 40` —
not further narrowed down (30 vs 40 bisection wasn't completed). Load
time roughly doubled from `-ngl 20` to `-ngl 30` (487s → 902s, more
layers now loading locally too, not just more risk on the RPC side).
Generation throughput was flat across both working configs (~1.5–2.5
tok/s) — not enough samples (2 runs, then 1 run) to call this a real
trend either way. **This boundary is specific to this session's hardware
pairing, model, and quantization** — it says nothing about where the
ceiling sits for a different model size, a different contributor's real
Vulkan heap budget, or (the actual fix) once the underlying Mesa/Vulkan
reclaim behavior itself is addressed rather than worked around by
tuning `-ngl` down.

Four real findings came out of one afternoon of genuinely separate-
hardware testing, none of which a same-host Docker fabric could have
produced: a same-host setup has no meaningful network latency to expose
the 90s-timeout gap, no reason to run mismatched `llama.cpp` builds on
each side, and — most fundamentally — no way to show a real "too large
for one node" capacity story, since the whole point of a single-host test
is that one host's resources are shared, not genuinely partitioned.
Three of the four findings (the dead-but-still-advertised contributor
process, the timeout, and the version-mismatch silent-corruption bug) are
correctness/resilience gaps, currently open and unfixed, worth
prioritizing before any claim that the distributed path is
production-ready beyond a single trusted operator running matched
binaries. The fourth (Result 3 above) is the payoff this whole
document-full of prior entries was building toward: real proof the
mechanism delivers on half of its core promise, with the other half
(usable speed, not just capacity) still open.

### Notes on Multi-Node Performance

- Two real cross-machine runs now exist: 2026-08-03 (synthetic
  `stage-worker` transport harness) and 2026-08-08 (real `ggml-rpc` path,
  native processes). Both used exactly 2 genuinely separate nodes. Scaling
  trends across 3+ genuinely separate nodes are still untested hypotheses,
  not measurements.
- **The single most important finding across every entry in this
  document**: version-mismatched `ggml-rpc` peers (2026-08-08's native
  two-machine entry) can silently corrupt inference output for larger
  models while the API reports healthy status throughout, and there is
  currently no version-compatibility check to catch this. Confirmed via a
  controlled before/after test on real hardware. Anyone running
  `contribute_compute`/`distributed_inference` across machines that don't
  share an exact `llama.cpp` build should treat output as unverified until
  this is fixed upstream in Ghostlink's own peer-handshake layer.
- Bridge-write latency (2026-08-03) tracks network RTT closely (10-16ms TCP
  vs. 8-14ms ICMP) — on that LAN, transport overhead is dominated by the
  network hop itself, not Ghostlink's framing/serialization.
- ggml-rpc's tensor split (2026-08-08) only moves real compute/memory off
  the coordinator when `-ngl` is configured to allow non-CPU-primary layer
  placement — a CPU-safety default of `-ngl 0` (as this repo's own Docker
  fabric ships) makes `--rpc`/`-ts` real-connection-but-inert. Worth
  checking deliberately, not assuming, on any new distributed-inference
  deployment.

---

## LLM-Shaped Workload Benchmarks — 2026-08-05

Every benchmark above this section moves a **16-`f32`-element (64-byte)
synthetic payload per token** — the pipeline-execution paths' historical
hardcoded stand-in, regardless of what `--exec-tokens` count is passed.
That's roughly **128x smaller than a real per-token activation** (a 7B-class
model's hidden state is ~4096 elements × 2 bytes in FP16/BF16 = 8192
bytes/token), so throughput/latency numbers above measure the transport
layer moving trivial packets, not data volumes an actual model would push.

This section fixes that: `TcpTransportConfig::elems_per_token` (new,
defaults to the historical 16 so every other benchmark/test above is
unaffected) lets the `flow` command's TCP/XDP execution paths carry a real
byte volume per token, controlled by two new env vars — `GHOSTLINK_FLOW_HIDDEN_DIM`
and `GHOSTLINK_FLOW_DTYPE_BYTES` (FP16/BF16 = 2) — and `ExecutionResult` now
reports `p99_token_latency_ms` and the raw per-token `token_latencies_ms`
samples (previously P95-only, aggregate-only).

**Opt-in, not default-on** — this took a real CI failure to get right the
first time. `flow` is also `production-gate.yml`'s smoke-test harness (SLO/
drift/canary/tail-latency gates, all calibrated against the small historical
payload); making realistic sizing the *default* broke that gate's throughput
threshold outright (measured: 4832 tok/s vs. its 10000 tok/s minimum) the
moment a tag push actually exercised it. `GHOSTLINK_FLOW_HIDDEN_DIM` is
unset by default — omit it and `flow` reproduces the historical 64-byte/token
payload exactly (verified: `simulated_hidden_dim`/`simulated_dtype_bytes`
report `null` in the JSON output, `bytes_per_token` reports `64`, and the
production-gate.yml smoke-test invocation — 256 tokens, micro_batch=4 — measures
134,249 tok/s unset vs. its 10,000 tok/s floor). Every command in this
section sets both env vars explicitly via `--hidden-dim`/`--dtype-bytes`.

**Important scope note**: the in-memory (`inmem`) transport mode has no
`TcpTransportConfig` at all and keeps the original 64-byte/token payload
regardless of these flags — its numbers below are **not** LLM-shaped,
included only as a same-session reference point. Only `tcp` mode below
actually moved 8192 bytes/token.

**Hardware** — same "laptop iGPU host" as the Full-Spectrum session above:
16 logical cores, 27.6 GB RAM, AMD Radeon 860M (integrated, 4.0 GB VRAM,
DirectML), Windows 11, `cargo build --release` (workspace `lto = "thin"`,
`codegen-units = 1`).

```bash
python scripts/flow_perf_snapshot.py --runs 3 --modes tcp inmem --release \
  --exec-tokens 4096 --micro-batch 32 --hidden-dim 4096 --dtype-bytes 2 --histogram
```

### Results — 7B-class payload (hidden_dim=4096, FP16/BF16, 8192 bytes/token), micro_batch=32, 3 runs each

| Tokens | Mode | Throughput (avg tok/s) | Throughput (min-max) | P95 (avg, ms) | P99 (avg, ms) | Bandwidth (avg GB/s) |
|---:|---|---:|---|---:|---:|---:|
| 4,096 | tcp | 14,934 | 14,530-15,448 | 260.1 | 266.2 | 0.122 |
| 4,096 | inmem* | 1,435,075 | 1,346,084-1,542,575 | 2.46 | 2.51 | 0.092* |
| 8,192 | tcp | 10,068 | 9,495-10,459 | 780.1 | 795.3 | 0.082 |
| 8,192 | inmem* | 1,634,012 | 1,407,971-1,769,216 | 4.59 | 4.72 | 0.105* |
| 16,384 | tcp | 13,084 | 12,488-13,508 | 1,192.4 | 1,214.4 | 0.107 |
| 16,384 | inmem* | 1,496,748 | 952,536-2,099,759 | 11.12 | 11.40 | 0.096* |
| 32,768 | tcp | 12,173 | 12,120-12,250 | 2,519.2 | 2,594.0 | 0.100 |
| 32,768 | inmem* | 2,046,090 | 1,968,036-2,103,452 | 14.75 | 15.14 | 0.131* |

\* `inmem` bandwidth is computed from its real 64-byte/token payload, not
8192 — it's the same tiny synthetic packet every other benchmark in this
file uses, listed here only for continuity with the tcp-mode rows above it.

**Bandwidth, GB/s: ~0.08-0.12 GB/s sustained on `tcp` loopback.** This is a
software-stack ceiling (framing, syscalls, loopback copy), not a network
measurement — **a loopback path has no real NIC in it**, so this number
cannot and does not answer "NIC-bound or CPU-bound?" That question needs a
real two-machine run over an actual link, like the Multi-Node Performance
section above (which measured real cross-machine bridge-write latency, not
bandwidth at this payload size — a real-NIC bandwidth run at 7B-class
payload sizes is open follow-up work, not done here).

**P99 vs. P95**: consistently 2-6% above P95 across all four token counts
(e.g. 32,768 tokens: 2519.2ms → 2594.0ms), not a heavy tail — no batch got
catastrophically stuck relative to the rest, on this loopback path, on this
host, at this concurrency.

**Throughput doesn't scale linearly with token count** (14,934 → 10,068 →
13,084 → 12,173 tok/s across 4K/8K/16K/32K) — flat-to-slightly-declining,
not the smooth curve a captive microbenchmark would produce; expect
run-to-run noise from a shared dev-machine host similar to the ~15-19%
stdev already documented in this file's Methodology-note section above.

### Comparative baseline — Ray actor-to-actor transfer

`scripts/ray_transfer_baseline.py` (new, not a project dependency —
`pip install ray`) moves the identical payload matrix (same token counts,
same 8192 bytes/token) between two local Ray actors via
`ray.get(actor.method.remote(payload))`, measuring the same metrics the
same way. This is a fair comparison to Ghostlink's own `tcp`-loopback
numbers above — **not** "Ray Serve" (a model-serving framework, a different
layer entirely), and **not** multi-node Ray (no second machine here either,
same single-host constraint as the Ghostlink `tcp` numbers). Ray 2.56.1 on
Python 3.10 (Ray does not yet support Python 3.13+; this repo's default
interpreter is 3.14, so this ran under a separate 3.10 install).

```bash
python scripts/ray_transfer_baseline.py --runs 3 --hidden-dim 4096 --dtype-bytes 2 \
  --exec-tokens 4096 --micro-batch 32 --histogram
```

| Tokens | Throughput (avg tok/s) | Throughput (min-max) | P95 (avg, ms) | P99 (avg, ms) | Bandwidth (avg GB/s) |
|---:|---:|---|---:|---:|---:|
| 4,096 | 16,342 | 15,810-17,114 | 2.73 | 6.50 | 0.134 |
| 8,192 | 17,905 | 17,432-18,542 | 3.03 | 3.69 | 0.147 |
| 16,384 | 14,989 | 9,367-19,182 | 2.82 | 3.45 | 0.123 |
| 32,768 | 17,995 | 16,268-19,938 | 2.76 | 3.25 | 0.147 |

**Genuinely surprising result, stated plainly**: at this batch size
(micro_batch=32, i.e. 256 KB per call), Ray's actor-to-actor throughput is
**comparable to or higher than** Ghostlink's own TCP-loopback numbers above
— the opposite of the naive assumption that a purpose-built Rust transport
would trivially beat a general-purpose Python actor framework. A smaller,
separate sanity check at micro_batch=8 (32 KB/call) showed the expected
result instead — Ghostlink ~8,060 tok/s vs. Ray ~3,491 tok/s — so Ray's
per-call overhead is real and dominates at small payloads, but gets
amortized away at larger ones. **Batch size, not backend choice alone,
determines which one wins here** — a call this repo's own numbers don't
support glossing over.

**The real P99-matters finding**: at 16,384 tokens, Ray's min/max throughput
across only 3 runs spans **9,367 to 19,182 tok/s (~2x)**, and its raw
latency histogram (`--histogram`) shows samples up to ~407ms against a P99
of only ~3.5ms — a rare but severe tail-latency outlier the P95/P99 averages
in the table above completely hide. Ghostlink's `tcp` numbers at the same
token count show ~4% min-max spread (12,488-13,508 tok/s) — meaningfully
more consistent run-to-run on this same host. This is exactly the kind of
signal mean/P95-only reporting misses, and the reason `token_latencies_ms`
(the raw per-token samples, not just precomputed percentiles) is now part
of the JSON output both scripts write.

### Reproduce this section

```bash
for tokens in 4096 8192 16384 32768; do
  python scripts/flow_perf_snapshot.py --runs 3 --modes tcp inmem --release \
    --exec-tokens "$tokens" --micro-batch 32 --hidden-dim 4096 --dtype-bytes 2 \
    --histogram --output-dir "tmp/perf_snapshot/$tokens"
  python scripts/ray_transfer_baseline.py --runs 3 --exec-tokens "$tokens" \
    --micro-batch 32 --histogram --output-dir "tmp/ray_baseline/$tokens"
done
```

Every number above came from these exact commands, this session, on the
hardware table above — falsifiable, run it yourself, same standard as the
rest of this document.

---

## Memory Requirements

Reference values for common quantization sizes, not measured on Ghostlink
hardware — use these for capacity planning, not as a Ghostlink-specific
throughput/latency claim.

### Model Memory Footprint

| Model | Parameters | Size (GGUF) | VRAM Required | System RAM Recommended |
|-------|-----------|-------------|---------------|------------------------|
| orca-mini | 3B | 1.7 GB | 2.0 GB | 8 GB |
| phi | 3B | 2.0 GB | 3.0 GB | 8 GB |
| mistral | 7B | 4.1 GB | 6.0 GB | 16 GB |
| neural-chat | 7B | 4.0 GB | 6.0 GB | 16 GB |
| llama2-7b | 7B | 3.8 GB | 5.5 GB | 16 GB |
| openhermes | 7B | 4.1 GB | 6.0 GB | 16 GB |
| llama2-13b | 13B | 7.3 GB | 10.0 GB | 32 GB |
| mistral-medium | 13B | 8.0 GB | 12.0 GB | 32 GB |
| codeup | 13B | 7.5 GB | 10.0 GB | 32 GB |
| dolphin-mixtral | 8x7B | 26.0 GB | 32.0 GB | 64 GB |
| llama2-70b | 70B | 39.0 GB | 48.0 GB | 128 GB |

---

## Benchmark Methodology

This section previously described a generic test configuration (a
24-core/RTX 4090 desktop, synthetic 10K-token prompts) that no number in
this document was actually produced on or with — it's been replaced with
the methodology the real entries above actually use.

### Tools

- `cargo bench -p ghostlink-core --bench criterion` — primitive-level
  microbenchmarks (ring buffer, protocol encode/decode, planning, cluster).
- `scripts/flow_perf_snapshot.py` — full pipeline throughput/latency,
  matched against `docs/PERF_BASELINE.json` / `PERF_BASELINE_STRESS.json`
  for drift detection (`scripts/check_perf_drift.py`).
- `scripts/remote_flow_benchmark.py` — real cross-process/cross-machine
  `stage-worker` benchmark harness.
- Full invocations and real output are in the Full-Spectrum Session
  Benchmark above; see [TESTING.md](TESTING.md) for the broader test/bench
  workflow this repo expects before a release.

### Test Procedure

1. Build in release mode (`cargo build --release`; workspace already sets
   `lto = "thin"`, `codegen-units = 1`).
2. Run the tool for the layer being measured (primitive vs. full pipeline
   vs. cross-process), matching `exec_tokens`/`micro_batch` to whatever
   baseline you're comparing against — mismatched parameters produce a
   number that looks like drift but isn't (see the Full-Spectrum session's
   own note on this).
3. Prefer multiple interleaved runs over a single run before drawing any
   conclusion — this repo's own dev hardware showed 5-80% run-to-run swings
   on some benchmarks (see "Methodology note: this host is noisy" above).

### Metrics

- **Throughput**: Tokens generated per second
- **Latency P50**: 50th percentile latency (median)
- **Latency P95**: 95th percentile latency (includes outliers)
- **Memory Usage**: Peak VRAM and system RAM usage

---

## Performance Tips

### Maximizing Throughput

1. **Use GPU when available**: GPU can provide 3-4x speedup over CPU
2. **Batch requests**: Larger batches improve throughput
3. **Use quantized models**: GGUF q4_0 or q5_0 quantization maintains quality while reducing memory

### Reducing Latency

1. **Keep model loaded**: Avoid loading/unloading frequently
2. **Use smaller models**: 3B-7B models have lower latency
3. **Enable GPU offloading**: Move layers to GPU when possible

### Multi-Node Optimization

Untested hypotheses pending the real multi-node LAN benchmark above — not
yet backed by measurement on this project:

1. Faster networks should help more nodes scale better.
2. Fewer nodes should mean less network overhead.
3. Balancing layers evenly across nodes should improve utilization.

---

## Future Improvements

### Planned Optimizations

- [ ] AF_XDP kernel-bypass implementation (requires Linux kernel 5.17+)
- [ ] Quantization-aware scheduling
- [ ] Layer parallelism optimization
- [ ] Continuous batching for variable-length inputs

### Known Limitations

- In-memory transport is currently the only implemented option
- AF_XDP requires Linux kernel 5.17+ and eBPF tooling
- Multi-node performance limited by network bandwidth

---

---

## KV cache microbenches

Five separate Criterion runs of `benches/kv_cache.rs` on the host below. They
are kept as five raw run logs rather than averaged: this repo's own
methodology note ("Methodology note: this host is noisy") says single-run
deltas are unreliable and the spread between runs *is* the signal. Each run
previously re-pasted the same header, NOTE, and baseline table; those are now
stated once.

- **Date**: 2026-03-31
- **OS / CPU**: Linux 6.8.0 x86_64, Intel(R) Xeon(R) CPU @ 2.30GHz (4 cores)
- **Rust / Build**: rustc 1.94.0,  release build
- **Command**: kv_cache/initialize/small

### Raw runs

#### Run 1

```
                        time:   [190.30 µs 192.26 µs 193.87 µs]
                        change: [-2.9907% +1.1902% +5.3370%] (p = 0.59 > 0.05)
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
kv_cache/write_kv_fill_256/small
                        time:   [221.72 µs 225.04 µs 228.01 µs]
                        change: [-3.9143% +3.8564% +12.432%] (p = 0.34 > 0.05)
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) low severe
kv_cache/write_kv_batch_64/small
                        time:   [22.926 µs 23.221 µs 23.493 µs]
                        change: [-10.892% -5.3673% +0.5704%] (p = 0.08 > 0.05)
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) low severe
  1 (1.00%) high mild
kv_cache/read_kv_mid/small
                        time:   [82.459 ns 82.512 ns 82.566 ns]
                        change: [-3.1302% -3.0307% -2.9343%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/read_range_1/small
                        time:   [86.005 ns 86.072 ns 86.137 ns]
                        change: [-1.7028% -1.2138% -0.9174%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_64/small
                        time:   [5.5372 µs 5.5956 µs 5.6652 µs]
                        change: [+3.9191% +5.7114% +7.5567%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
kv_cache/read_range_1024/small
                        time:   [138.87 µs 139.79 µs 140.65 µs]
                        change: [+2.5985% +3.2229% +3.8554%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 24 outliers among 100 measurements (24.00%)
  6 (6.00%) low mild
  13 (13.00%) high mild
  5 (5.00%) high severe
kv_cache/decode_step_256/small
                        time:   [3.0426 ms 3.0541 ms 3.0661 ms]
                        change: [-3.0511% -2.5755% -2.1022%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
kv_cache/concurrent_4_readers_256/small
                        time:   [29.691 µs 29.926 µs 30.193 µs]
                        change: [+10.496% +15.082% +20.439%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 24 outliers among 100 measurements (24.00%)
  19 (19.00%) high mild
  5 (5.00%) high severe
kv_cache/initialize/default
                        time:   [199.08 µs 204.30 µs 211.16 µs]
                        change: [+0.7658% +4.7528% +8.6017%] (p = 0.02 < 0.05)
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
kv_cache/write_kv_fill_256/default
                        time:   [230.96 µs 235.83 µs 239.93 µs]
                        change: [-1.5464% +1.3502% +4.5756%] (p = 0.40 > 0.05)
                        No change in performance detected.
kv_cache/write_kv_batch_64/default
                        time:   [23.202 µs 23.603 µs 23.996 µs]
                        change: [-14.376% -12.169% -9.9916%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
kv_cache/read_kv_mid/default
                        time:   [81.193 ns 81.268 ns 81.385 ns]
                        change: [-3.0182% -2.6179% -1.9948%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  4 (4.00%) high mild
  13 (13.00%) high severe
kv_cache/read_range_1/default
                        time:   [81.916 ns 81.966 ns 82.018 ns]
                        change: [-2.2782% -2.1649% -2.0492%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_64/default
                        time:   [5.8477 µs 5.9339 µs 6.0292 µs]
                        change: [+31.421% +34.038% +36.600%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/read_range_1024/default
                        time:   [136.09 µs 137.07 µs 138.13 µs]
                        change: [-1.9445% -1.1534% -0.3500%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
kv_cache/decode_step_256/default
                        time:   [3.3062 ms 3.3181 ms 3.3300 ms]
                        change: [+8.5213% +8.9634% +9.4070%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/concurrent_4_readers_256/default
                        time:   [28.390 µs 28.527 µs 28.668 µs]
                        change: [-19.727% -16.794% -13.699%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe
kv_cache/initialize/wide
                        time:   [18.180 µs 19.652 µs 20.920 µs]
                        change: [+16.763% +25.656% +34.179%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
kv_cache/write_kv_fill_256/wide
                        time:   [5.7655 ms 5.7918 ms 5.8194 ms]
                        change: [-3.4544% -2.5479% -1.6662%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
kv_cache/write_kv_batch_64/wide
                        time:   [1.3586 ms 1.3606 ms 1.3626 ms]
                        change: [-0.6507% -0.4416% -0.2465%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
kv_cache/read_kv_mid/wide
                        time:   [1.3040 µs 1.3225 µs 1.3430 µs]
                        change: [-7.1605% -5.8861% -4.4482%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) high mild
  6 (6.00%) high severe
kv_cache/read_range_1/wide
                        time:   [1.2851 µs 1.3045 µs 1.3239 µs]
                        change: [-8.1562% -6.9308% -5.7144%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
kv_cache/read_range_64/wide
                        time:   [143.43 µs 144.80 µs 146.12 µs]
                        change: [+5.9715% +6.8764% +7.7859%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
kv_cache/read_range_1024/wide
                        time:   [32.299 ms 32.450 ms 32.631 ms]
                        change: [+0.2079% +0.7443% +1.2910%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe
kv_cache/decode_step_256/wide
                        time:   [112.58 ms 113.08 ms 113.60 ms]
                        change: [+3.4193% +3.9335% +4.4432%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
kv_cache/concurrent_4_readers_256/wide
                        time:   [1.3696 ms 1.3806 ms 1.3917 ms]
                        change: [-11.262% -8.6108% -5.8873%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
```

#### Run 2

```
                        time:   [184.38 µs 186.18 µs 187.79 µs]
                        change: [-6.4867% -2.3638% +1.6278%] (p = 0.28 > 0.05)
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
kv_cache/write_kv_fill_256/small
                        time:   [216.27 µs 219.88 µs 223.14 µs]
                        change: [-10.334% -2.5670% +5.3811%] (p = 0.52 > 0.05)
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) low severe
kv_cache/write_kv_batch_64/small
                        time:   [21.725 µs 22.100 µs 22.449 µs]
                        change: [-10.843% -5.2802% +0.4396%] (p = 0.08 > 0.05)
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) low severe
kv_cache/read_kv_mid/small
                        time:   [85.133 ns 85.156 ns 85.181 ns]
                        change: [+3.3751% +3.4926% +3.6267%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
kv_cache/read_range_1/small
                        time:   [84.354 ns 84.411 ns 84.470 ns]
                        change: [-1.9529% -1.8227% -1.7022%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low severe
  1 (1.00%) high mild
  3 (3.00%) high severe
kv_cache/read_range_64/small
                        time:   [5.2167 µs 5.3019 µs 5.3950 µs]
                        change: [-10.579% -9.0968% -7.5337%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) high mild
  2 (2.00%) high severe
kv_cache/read_range_1024/small
                        time:   [135.64 µs 136.35 µs 137.08 µs]
                        change: [-1.2627% -0.5366% +0.3013%] (p = 0.18 > 0.05)
                        No change in performance detected.
kv_cache/decode_step_256/small
                        time:   [2.8531 ms 2.8601 ms 2.8672 ms]
                        change: [-6.7956% -6.3521% -5.9238%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
kv_cache/concurrent_4_readers_256/small
                        time:   [28.874 µs 28.994 µs 29.126 µs]
                        change: [-16.126% -12.516% -8.8299%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
kv_cache/initialize/default
                        time:   [183.67 µs 184.97 µs 186.20 µs]
                        change: [-10.885% -7.3219% -3.9515%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
kv_cache/write_kv_fill_256/default
                        time:   [218.89 µs 222.58 µs 225.89 µs]
                        change: [-6.4576% -3.8892% -1.2478%] (p = 0.01 < 0.05)
                        Performance has improved.
kv_cache/write_kv_batch_64/default
                        time:   [24.371 µs 24.747 µs 25.090 µs]
                        change: [+2.0365% +4.7862% +7.5012%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
kv_cache/read_kv_mid/default
                        time:   [81.404 ns 81.455 ns 81.512 ns]
                        change: [-0.9962% -0.4954% -0.0802%] (p = 0.03 < 0.05)
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_1/default
                        time:   [81.649 ns 81.690 ns 81.736 ns]
                        change: [-0.2913% -0.1824% -0.0713%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_64/default
                        time:   [5.6652 µs 5.7611 µs 5.8673 µs]
                        change: [-7.2487% -5.6236% -3.9157%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  9 (9.00%) high mild
  2 (2.00%) high severe
kv_cache/read_range_1024/default
                        time:   [138.57 µs 139.48 µs 140.45 µs]
                        change: [+1.6324% +2.5130% +3.4211%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/decode_step_256/default
                        time:   [3.3431 ms 3.3553 ms 3.3675 ms]
                        change: [+0.6160% +1.1209% +1.6160%] (p = 0.00 < 0.05)
                        Change within noise threshold.
kv_cache/concurrent_4_readers_256/default
                        time:   [28.640 µs 28.745 µs 28.852 µs]
                        change: [-0.5555% +0.7369% +1.9376%] (p = 0.26 > 0.05)
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  8 (8.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
kv_cache/initialize/wide
                        time:   [13.642 µs 13.827 µs 14.016 µs]
                        change: [-21.380% -15.899% -9.5857%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
kv_cache/write_kv_fill_256/wide
                        time:   [7.0279 ms 7.4171 ms 7.8156 ms]
                        change: [+21.539% +28.062% +35.173%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/write_kv_batch_64/wide
                        time:   [1.3687 ms 1.3706 ms 1.3726 ms]
                        change: [+0.8879% +1.0856% +1.3192%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
kv_cache/read_kv_mid/wide
                        time:   [1.2896 µs 1.3032 µs 1.3182 µs]
                        change: [-1.8307% -0.4250% +1.0237%] (p = 0.57 > 0.05)
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
kv_cache/read_range_1/wide
                        time:   [1.2875 µs 1.3039 µs 1.3212 µs]
                        change: [+0.0948% +1.4491% +2.6988%] (p = 0.03 < 0.05)
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  8 (8.00%) high mild
  3 (3.00%) high severe
kv_cache/read_range_64/wide
                        time:   [138.96 µs 139.64 µs 140.39 µs]
                        change: [-3.6662% -2.8680% -2.0638%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/read_range_1024/wide
                        time:   [32.449 ms 32.501 ms 32.553 ms]
                        change: [-0.4199% +0.1564% +0.6533%] (p = 0.59 > 0.05)
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
kv_cache/decode_step_256/wide
                        time:   [111.31 ms 111.58 ms 111.86 ms]
                        change: [-1.8344% -1.3219% -0.8056%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
kv_cache/concurrent_4_readers_256/wide
                        time:   [1.3594 ms 1.3643 ms 1.3696 ms]
                        change: [-1.0249% +0.1821% +1.4888%] (p = 0.78 > 0.05)
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
```

#### Run 3

```
                        time:   [185.09 µs 186.81 µs 188.28 µs]
                        change: [-3.6780% +0.4235% +4.7030%] (p = 0.85 > 0.05)
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
kv_cache/write_kv_fill_256/small
                        time:   [222.41 µs 226.12 µs 229.35 µs]
                        change: [-5.5736% +1.9935% +10.197%] (p = 0.63 > 0.05)
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) low severe
kv_cache/write_kv_batch_64/small
                        time:   [22.454 µs 22.944 µs 23.387 µs]
                        change: [-3.0935% +2.7183% +9.2967%] (p = 0.39 > 0.05)
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) low severe
  4 (4.00%) high mild
kv_cache/read_kv_mid/small
                        time:   [81.503 ns 81.561 ns 81.621 ns]
                        change: [-4.5783% -4.4468% -4.3237%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_1/small
                        time:   [81.922 ns 81.983 ns 82.048 ns]
                        change: [-2.8955% -2.7810% -2.6637%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_64/small
                        time:   [5.2140 µs 5.3147 µs 5.4200 µs]
                        change: [+0.3599% +2.5255% +4.6647%] (p = 0.02 < 0.05)
                        Change within noise threshold.
kv_cache/read_range_1024/small
                        time:   [183.54 µs 201.23 µs 217.01 µs]
                        change: [+12.969% +19.828% +27.476%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 24 outliers among 100 measurements (24.00%)
  2 (2.00%) high mild
  22 (22.00%) high severe
kv_cache/decode_step_256/small
                        time:   [2.8818 ms 2.8896 ms 2.8980 ms]
                        change: [+0.6534% +1.0320% +1.4225%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  11 (11.00%) high mild
  1 (1.00%) high severe
kv_cache/concurrent_4_readers_256/small
                        time:   [28.336 µs 28.469 µs 28.611 µs]
                        change: [-2.0874% -0.6571% +0.7792%] (p = 0.40 > 0.05)
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
kv_cache/initialize/default
                        time:   [189.59 µs 191.90 µs 193.78 µs]
                        change: [-0.6266% +2.0315% +4.9476%] (p = 0.17 > 0.05)
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low mild
kv_cache/write_kv_fill_256/default
                        time:   [221.18 µs 225.07 µs 228.41 µs]
                        change: [-1.8865% +0.4207% +2.7711%] (p = 0.72 > 0.05)
                        No change in performance detected.
kv_cache/write_kv_batch_64/default
                        time:   [22.894 µs 23.165 µs 23.418 µs]
                        change: [-6.6284% -4.5757% -2.3955%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
kv_cache/read_kv_mid/default
                        time:   [81.024 ns 81.064 ns 81.106 ns]
                        change: [-0.6803% -0.5909% -0.5103%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low severe
  2 (2.00%) high mild
kv_cache/read_range_1/default
                        time:   [81.963 ns 82.042 ns 82.120 ns]
                        change: [+0.1222% +0.2346% +0.3404%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
kv_cache/read_range_64/default
                        time:   [5.1243 µs 5.2454 µs 5.3689 µs]
                        change: [-13.243% -11.237% -9.2579%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_1024/default
                        time:   [134.50 µs 135.19 µs 135.99 µs]
                        change: [-3.4019% -2.6092% -1.8103%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
kv_cache/decode_step_256/default
                        time:   [3.2053 ms 3.2144 ms 3.2238 ms]
                        change: [-4.6356% -4.2003% -3.7736%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
kv_cache/concurrent_4_readers_256/default
                        time:   [29.104 µs 29.185 µs 29.271 µs]
                        change: [+0.0841% +1.3996% +2.7056%] (p = 0.04 < 0.05)
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  5 (5.00%) high severe
kv_cache/initialize/wide
                        time:   [13.384 µs 13.550 µs 13.718 µs]
                        change: [-6.3104% -2.3699% +0.7731%] (p = 0.23 > 0.05)
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
kv_cache/write_kv_fill_256/wide
                        time:   [5.8282 ms 5.8581 ms 5.8903 ms]
                        change: [-25.046% -21.019% -16.660%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
kv_cache/write_kv_batch_64/wide
                        time:   [1.4204 ms 1.4401 ms 1.4666 ms]
                        change: [+5.3967% +6.6115% +8.0318%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
kv_cache/read_kv_mid/wide
                        time:   [1.3938 µs 1.4151 µs 1.4372 µs]
                        change: [+8.4980% +10.427% +12.282%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
kv_cache/read_range_1/wide
                        time:   [1.3508 µs 1.3638 µs 1.3793 µs]
                        change: [+6.9245% +8.8998% +10.906%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
kv_cache/read_range_64/wide
                        time:   [135.19 µs 136.25 µs 137.43 µs]
                        change: [-3.2872% -2.4511% -1.5418%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/read_range_1024/wide
                        time:   [33.219 ms 33.311 ms 33.408 ms]
                        change: [+2.1672% +2.4942% +2.8119%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
kv_cache/decode_step_256/wide
                        time:   [112.18 ms 112.49 ms 112.79 ms]
                        change: [+0.4339% +0.8086% +1.1918%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
kv_cache/concurrent_4_readers_256/wide
                        time:   [1.5851 ms 1.6613 ms 1.7263 ms]
                        change: [+4.7282% +8.0187% +11.463%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
```

#### Run 4

```
                        time:   [187.26 µs 189.36 µs 191.11 µs]
                        change: [-4.1266% -0.0746% +4.1839%] (p = 0.98 > 0.05)
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
kv_cache/write_kv_fill_256/small
                        time:   [218.52 µs 221.34 µs 223.67 µs]
                        change: [-8.7989% -1.0379% +7.0616%] (p = 0.80 > 0.05)
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) low severe
kv_cache/write_kv_batch_64/small
                        time:   [23.906 µs 24.248 µs 24.557 µs]
                        change: [+1.1048% +7.4267% +14.200%] (p = 0.03 < 0.05)
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  7 (7.00%) low severe
  2 (2.00%) high mild
kv_cache/read_kv_mid/small
                        time:   [83.222 ns 83.311 ns 83.429 ns]
                        change: [+2.0299% +2.1523% +2.2951%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
kv_cache/read_range_1/small
                        time:   [84.613 ns 84.700 ns 84.825 ns]
                        change: [+3.0866% +3.2240% +3.3658%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
kv_cache/read_range_64/small
                        time:   [5.2444 µs 5.3845 µs 5.5577 µs]
                        change: [+13.056% +17.964% +23.260%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/read_range_1024/small
                        time:   [136.55 µs 137.77 µs 138.91 µs]
                        change: [-22.229% -17.231% -11.937%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/decode_step_256/small
                        time:   [3.1402 ms 3.1527 ms 3.1656 ms]
                        change: [+8.6062% +9.1058% +9.6460%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/concurrent_4_readers_256/small
                        time:   [28.680 µs 28.780 µs 28.889 µs]
                        change: [-1.1275% +0.2566% +1.5159%] (p = 0.73 > 0.05)
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
kv_cache/initialize/default
                        time:   [189.45 µs 191.34 µs 192.98 µs]
                        change: [-2.2167% +0.6411% +3.5228%] (p = 0.66 > 0.05)
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
kv_cache/write_kv_fill_256/default
                        time:   [230.82 µs 235.05 µs 238.65 µs]
                        change: [+0.7315% +3.5531% +6.2202%] (p = 0.01 < 0.05)
                        Change within noise threshold.
kv_cache/write_kv_batch_64/default
                        time:   [24.556 µs 24.926 µs 25.263 µs]
                        change: [+2.2248% +4.5399% +6.8976%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low severe
  9 (9.00%) low mild
  1 (1.00%) high mild
kv_cache/read_kv_mid/default
                        time:   [81.335 ns 81.395 ns 81.458 ns]
                        change: [+0.3549% +0.4377% +0.5260%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) low mild
  5 (5.00%) high mild
kv_cache/read_range_1/default
                        time:   [81.759 ns 81.798 ns 81.840 ns]
                        change: [-0.2934% -0.1883% -0.0876%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe
kv_cache/read_range_64/default
                        time:   [4.7031 µs 4.8133 µs 4.9222 µs]
                        change: [-12.377% -9.9637% -7.3597%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/read_range_1024/default
                        time:   [155.25 µs 166.07 µs 179.37 µs]
                        change: [+18.246% +27.101% +36.890%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/decode_step_256/default
                        time:   [2.9634 ms 2.9733 ms 2.9834 ms]
                        change: [-7.9156% -7.4987% -7.1001%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/concurrent_4_readers_256/default
                        time:   [28.865 µs 28.942 µs 29.021 µs]
                        change: [-1.7802% -0.4085% +1.0237%] (p = 0.57 > 0.05)
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
kv_cache/initialize/wide
                        time:   [14.060 µs 14.246 µs 14.428 µs]
                        change: [+0.9798% +3.3571% +5.7229%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
kv_cache/write_kv_fill_256/wide
                        time:   [5.8738 ms 5.8949 ms 5.9172 ms]
                        change: [-0.0117% +0.6293% +1.2621%] (p = 0.06 > 0.05)
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
kv_cache/write_kv_batch_64/wide
                        time:   [1.3906 ms 1.3930 ms 1.3957 ms]
                        change: [-5.8322% -4.5558% -3.4568%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
kv_cache/read_kv_mid/wide
                        time:   [1.3254 µs 1.3397 µs 1.3561 µs]
                        change: [-7.7173% -6.0901% -4.5330%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
kv_cache/read_range_1/wide
                        time:   [1.3264 µs 1.3416 µs 1.3592 µs]
                        change: [-6.2086% -4.4601% -2.7535%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) high mild
  2 (2.00%) high severe
kv_cache/read_range_64/wide
                        time:   [136.83 µs 137.54 µs 138.28 µs]
                        change: [-1.5973% -0.7945% +0.0468%] (p = 0.06 > 0.05)
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  11 (11.00%) high mild
kv_cache/read_range_1024/wide
                        time:   [32.320 ms 32.374 ms 32.430 ms]
                        change: [-3.1283% -2.8143% -2.4939%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
kv_cache/decode_step_256/wide
                        time:   [114.83 ms 119.19 ms 124.01 ms]
                        change: [+2.3239% +5.9632% +10.268%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 15 outliers among 100 measurements (15.00%)
  15 (15.00%) high severe
kv_cache/concurrent_4_readers_256/wide
                        time:   [1.3874 ms 1.3947 ms 1.4020 ms]
                        change: [-10.250% -7.3692% -4.4935%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
```

#### Run 5

```
                        time:   [176.32 µs 177.25 µs 178.14 µs]
                        change: [-3.4925% +1.5403% +6.8888%] (p = 0.57 > 0.05)
                        No change in performance detected.
Found 7 outliers among 50 measurements (14.00%)
  1 (2.00%) low severe
  4 (8.00%) high mild
  2 (4.00%) high severe
kv_cache/write_kv_fill_256/small
                        time:   [195.29 µs 197.32 µs 198.99 µs]
                        change: [-11.287% +0.3456% +14.596%] (p = 0.96 > 0.05)
                        No change in performance detected.
Found 7 outliers among 50 measurements (14.00%)
  7 (14.00%) low severe
kv_cache/write_kv_batch_64/small
                        time:   [21.713 µs 21.913 µs 22.090 µs]
                        change: [-11.180% -2.1556% +7.2646%] (p = 0.65 > 0.05)
                        No change in performance detected.
Found 5 outliers among 50 measurements (10.00%)
  4 (8.00%) low severe
  1 (2.00%) high mild
kv_cache/read_kv_mid/small
                        time:   [82.806 ns 82.882 ns 82.967 ns]
                        change: [-11.634% -11.525% -11.382%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 50 measurements (8.00%)
  3 (6.00%) high mild
  1 (2.00%) high severe
kv_cache/with_read_kv_mid_zero_copy/small
                        time:   [21.881 ns 21.894 ns 21.910 ns]
                        change: [-0.7151% -0.3840% -0.1342%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low mild
  1 (2.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_1/small
                        time:   [117.34 ns 119.25 ns 121.12 ns]
                        change: [+24.184% +25.850% +27.393%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/with_read_range_1_zero_copy/small
                        time:   [21.910 ns 21.922 ns 21.935 ns]
                        change: [+0.0430% +0.1457% +0.2782%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low mild
  1 (2.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_into_1_caller_buf/small
                        time:   [57.816 ns 57.850 ns 57.887 ns]
                        change: [+7.1090% +7.2823% +7.4519%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low severe
  2 (4.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_64/small
                        time:   [5.0489 µs 5.1637 µs 5.3000 µs]
                        change: [-4.2276% -0.8992% +2.4935%] (p = 0.61 > 0.05)
                        No change in performance detected.
kv_cache/with_read_range_64_zero_copy/small
                        time:   [21.906 ns 21.925 ns 21.944 ns]
                        change: [-0.1187% +0.0256% +0.1772%] (p = 0.75 > 0.05)
                        No change in performance detected.
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) low severe
  1 (2.00%) high severe
kv_cache/read_range_into_64_caller_buf/small
                        time:   [4.8477 µs 5.0045 µs 5.1743 µs]
                        change: [-27.941% -23.140% -17.477%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 7 outliers among 50 measurements (14.00%)
  5 (10.00%) high mild
  2 (4.00%) high severe
kv_cache/read_range_1024/small
                        time:   [136.27 µs 136.77 µs 137.37 µs]
                        change: [-2.3722% -1.3800% -0.3460%] (p = 0.01 < 0.05)
                        Change within noise threshold.
kv_cache/with_read_range_1024_zero_copy/small
                        time:   [21.881 ns 21.897 ns 21.915 ns]
                        change: [-0.2367% -0.1320% -0.0264%] (p = 0.02 < 0.05)
                        Change within noise threshold.
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low mild
  2 (4.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_into_1024_caller_buf/small
                        time:   [138.23 µs 140.18 µs 142.39 µs]
                        change: [-3.7205% -2.6046% -1.4424%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 19 outliers among 50 measurements (38.00%)
  8 (16.00%) low severe
  11 (22.00%) high severe
kv_cache/decode_step_256/small
                        time:   [2.8656 ms 2.8766 ms 2.8887 ms]
                        change: [-1.6625% -1.0681% -0.4511%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 10 outliers among 50 measurements (20.00%)
  8 (16.00%) high mild
  2 (4.00%) high severe
kv_cache/decode_step_256_zero_copy/small
                        time:   [203.02 µs 206.33 µs 209.14 µs]
                        change: [-12.352% -0.6410% +13.041%] (p = 0.92 > 0.05)
                        No change in performance detected.
Found 7 outliers among 50 measurements (14.00%)
  7 (14.00%) low severe
kv_cache/concurrent_4_readers_256/small
                        time:   [25.037 µs 25.187 µs 25.347 µs]
                        change: [-0.7948% +0.7168% +2.2380%] (p = 0.36 > 0.05)
                        No change in performance detected.
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low mild
  1 (2.00%) high mild
  2 (4.00%) high severe
kv_cache/concurrent_4_readers_256_zero_copy/small
                        time:   [342.43 ns 343.49 ns 344.42 ns]
                        change: [+0.4106% +1.3237% +2.3211%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 5 outliers among 50 measurements (10.00%)
  2 (4.00%) low mild
  1 (2.00%) high mild
  2 (4.00%) high severe

kv_cache/initialize/default
                        time:   [174.12 µs 176.18 µs 177.95 µs]
                        change: [-5.1652% -2.0553% +1.2245%] (p = 0.22 > 0.05)
                        No change in performance detected.
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low severe
  2 (4.00%) low mild
kv_cache/write_kv_fill_256/default
                        time:   [198.59 µs 200.39 µs 201.95 µs]
                        change: [-5.3406% -3.6055% -1.8093%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 50 measurements (6.00%)
  3 (6.00%) low mild
kv_cache/write_kv_batch_64/default
                        time:   [22.842 µs 23.304 µs 23.668 µs]
                        change: [-7.1501% -3.5978% -0.3055%] (p = 0.05 > 0.05)
                        No change in performance detected.
kv_cache/read_kv_mid/default
                        time:   [80.894 ns 80.944 ns 81.004 ns]
                        change: [-4.5388% -4.3978% -4.2423%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild
kv_cache/with_read_kv_mid_zero_copy/default
                        time:   [21.862 ns 21.871 ns 21.882 ns]
                        change: [-0.3764% -0.2769% -0.1897%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) low severe
  1 (2.00%) high mild
kv_cache/read_range_1/default
                        time:   [81.597 ns 81.674 ns 81.752 ns]
                        change: [-3.5407% -3.3989% -3.2536%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 50 measurements (10.00%)
  1 (2.00%) low severe
  2 (4.00%) high mild
  2 (4.00%) high severe
kv_cache/with_read_range_1_zero_copy/default
                        time:   [21.882 ns 21.898 ns 21.917 ns]
                        change: [-0.2513% -0.1148% +0.0314%] (p = 0.12 > 0.05)
                        No change in performance detected.
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low mild
  2 (4.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_into_1_caller_buf/default
                        time:   [74.732 ns 75.916 ns 76.925 ns]
                        change: [+18.327% +24.141% +29.328%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/read_range_64/default
                        time:   [4.2799 µs 4.4867 µs 4.6969 µs]
                        change: [-19.458% -16.392% -13.194%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 50 measurements (10.00%)
  4 (8.00%) high mild
  1 (2.00%) high severe
kv_cache/with_read_range_64_zero_copy/default
                        time:   [21.858 ns 21.865 ns 21.874 ns]
                        change: [-0.6819% -0.3723% -0.1630%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_into_64_caller_buf/default
                        time:   [4.1988 µs 4.3845 µs 4.5850 µs]
                        change: [-18.437% -15.078% -11.391%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 50 measurements (12.00%)
  6 (12.00%) high mild
kv_cache/read_range_1024/default
                        time:   [141.40 µs 143.13 µs 145.54 µs]
                        change: [+4.6574% +6.6138% +8.5650%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/with_read_range_1024_zero_copy/default
                        time:   [21.895 ns 21.912 ns 21.930 ns]
                        change: [-0.2131% -0.0572% +0.0924%] (p = 0.50 > 0.05)
                        No change in performance detected.
Found 3 outliers among 50 measurements (6.00%)
  2 (4.00%) high mild
  1 (2.00%) high severe
kv_cache/read_range_into_1024_caller_buf/default
                        time:   [143.91 µs 145.27 µs 147.15 µs]
                        change: [+6.4486% +7.7155% +8.9216%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/decode_step_256/default
                        time:   [2.8613 ms 2.8650 ms 2.8688 ms]
                        change: [-10.216% -9.8813% -9.5440%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low mild
  3 (6.00%) high mild
kv_cache/decode_step_256_zero_copy/default
                        time:   [206.71 µs 209.94 µs 214.40 µs]
                        change: [-0.4845% +1.5705% +3.7690%] (p = 0.15 > 0.05)
                        No change in performance detected.
Found 4 outliers among 50 measurements (8.00%)
  2 (4.00%) low mild
  1 (2.00%) high mild
  1 (2.00%) high severe
kv_cache/concurrent_4_readers_256/default
                        time:   [25.003 µs 25.107 µs 25.226 µs]
                        change: [-3.1723% -1.4611% +0.1451%] (p = 0.10 > 0.05)
                        No change in performance detected.
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe
kv_cache/concurrent_4_readers_256_zero_copy/default
                        time:   [403.54 ns 405.77 ns 407.94 ns]
                        change: [+17.280% +18.098% +18.859%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 5 outliers among 50 measurements (10.00%)
  1 (2.00%) low severe
  4 (8.00%) low mild

kv_cache/initialize/wide
                        time:   [14.286 µs 14.519 µs 14.659 µs]
                        change: [-6.0658% -2.1036% +2.6367%] (p = 0.40 > 0.05)
                        No change in performance detected.
kv_cache/write_kv_fill_256/wide
                        time:   [5.6523 ms 5.6836 ms 5.7086 ms]
                        change: [-2.7970% -1.8809% -1.0455%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
kv_cache/write_kv_batch_64/wide
                        time:   [1.3659 ms 1.3729 ms 1.3821 ms]
                        change: [+0.0977% +0.5428% +1.0366%] (p = 0.04 < 0.05)
                        Change within noise threshold.
kv_cache/read_kv_mid/wide
                        time:   [1.2634 µs 1.3198 µs 1.3656 µs]
                        change: [-5.7731% -2.1854% +1.5201%] (p = 0.28 > 0.05)
                        No change in performance detected.
kv_cache/with_read_kv_mid_zero_copy/wide
                        time:   [21.866 ns 21.875 ns 21.887 ns]
                        change: [-0.1945% -0.0652% +0.0389%] (p = 0.34 > 0.05)
                        No change in performance detected.
kv_cache/read_range_1/wide
                        time:   [1.2348 µs 1.2434 µs 1.2556 µs]
                        change: [-2.7930% -2.2239% -1.6419%] (p = 0.00 < 0.05)
                        Performance has improved.
kv_cache/with_read_range_1_zero_copy/wide
                        time:   [21.916 ns 21.945 ns 21.970 ns]
                        change: [-0.0042% +0.1586% +0.3144%] (p = 0.10 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
kv_cache/read_range_into_1_caller_buf/wide
                        time:   [1.0985 µs 1.1254 µs 1.1702 µs]
                        change: [-5.2358% -0.8009% +4.0686%] (p = 0.75 > 0.05)
                        No change in performance detected.
kv_cache/read_range_64/wide
                        time:   [146.16 µs 146.64 µs 147.76 µs]
                        change: [-3.8714% -1.8465% +0.3581%] (p = 0.13 > 0.05)
                        No change in performance detected.
kv_cache/with_read_range_64_zero_copy/wide
                        time:   [21.856 ns 21.861 ns 21.871 ns]
                        change: [-0.2152% -0.0640% +0.1231%] (p = 0.52 > 0.05)
                        No change in performance detected.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
kv_cache/read_range_into_64_caller_buf/wide
                        time:   [137.84 µs 139.50 µs 140.81 µs]
                        change: [-7.3485% -6.2496% -4.9442%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
kv_cache/read_range_1024/wide
                        time:   [31.479 ms 31.614 ms 31.712 ms]
                        change: [-3.6239% -3.1065% -2.5965%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
kv_cache/with_read_range_1024_zero_copy/wide
                        time:   [21.879 ns 21.886 ns 21.898 ns]
                        change: [-0.3304% -0.1873% -0.0606%] (p = 0.02 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
kv_cache/read_range_into_1024_caller_buf/wide
                        time:   [6.3891 ms 6.3955 ms 6.4109 ms]
                        change: [-4.2544% -3.5022% -2.7037%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
kv_cache/decode_step_256/wide
                        time:   [119.72 ms 134.28 ms 148.38 ms]
                        change: [+11.804% +25.233% +38.088%] (p = 0.00 < 0.05)
                        Performance has regressed.
kv_cache/decode_step_256_zero_copy/wide
                        time:   [5.7357 ms 5.7868 ms 5.8191 ms]
                        change: [-1.0707% -0.1131% +0.8272%] (p = 0.82 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
kv_cache/concurrent_4_readers_256/wide
                        time:   [1.3215 ms 1.3244 ms 1.3295 ms]
                        change: [-2.0364% -1.0641% +0.1715%] (p = 0.10 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
kv_cache/concurrent_4_readers_256_zero_copy/wide
                        time:   [341.75 ns 344.69 ns 347.29 ns]
                        change: [+1.2195% +1.9775% +2.8231%] (p = 0.00 < 0.05)
                        Performance has regressed.
```

> [!NOTE]
> These numbers measure isolated KV cache primitive operations and memory copies. They do not represent end-to-end LLM model inference tokens per second (tok/s).


### Baseline (BEFORE) Measurements (pre-zero-copy, owned-read API)
| Workload |  (1024, 4x64) |  (8192, 4x64) |  (2048, 32x128) | Notes |
|---|---|---|---|---|
|  | 182.17 µs | 182.50 µs | 13.10 µs | Upfront buffer allocation |
|  | 208.07 µs | 212.73 µs | 5.73 ms | Sequential fill 256 tokens |
|  | 23.14 µs | 22.09 µs | 1.37 ms | Batch write 64 tokens under 1 lock |
|  | 82.54 ns | 81.56 ns | 1.28 µs | Owned read ( Vec alloc) |
|  | 82.53 ns | 81.68 ns | 1.29 µs | Owned range read 1 token (Vec alloc) |
|  | 5.43 µs | 5.16 µs | 141.32 µs | Owned range read 64 tokens |
|  | 137.68 µs | 126.44 µs | 32.59 ms | Owned range read 1024 tokens |
|  | 3.10 ms | 3.27 ms | 108.89 ms | Write 1 + Read 0..t+1 over 256 steps |
|  | 29.14 µs | 40.93 µs | 1.36 ms | 4 reader threads looping range(0..256) |
---


### Baseline (BEFORE) Measurements (with zero-copy read APIs)
| Workload |  (1024, 4x64) |  (8192, 4x64) |  (2048, 32x128) | Notes |
|---|---|---|---|---|
|  | 182.17 µs | 182.50 µs | 13.10 µs | Upfront buffer allocation |
|  | 208.07 µs | 212.73 µs | 5.73 ms | Sequential fill 256 tokens |
|  | 23.14 µs | 22.09 µs | 1.37 ms | Batch write 64 tokens under 1 lock |
|  | 82.54 ns | 81.56 ns | 1.28 µs | Owned read ( Vec alloc) |
|  | 82.53 ns | 81.68 ns | 1.29 µs | Owned range read 1 token (Vec alloc) |
|  | 5.43 µs | 5.16 µs | 141.32 µs | Owned range read 64 tokens |
|  | 137.68 µs | 126.44 µs | 32.59 ms | Owned range read 1024 tokens |
|  | 3.10 ms | 3.27 ms | 108.89 ms | Write 1 + Read 0..t+1 over 256 steps |
|  | 29.14 µs | 40.93 µs | 1.36 ms | 4 reader threads looping range(0..256) |
### Optimized (AFTER) Measurements
| Workload |  (1024, 4x64) |  (8192, 4x64) |  (2048, 32x128) | Notes |
|---|---|---|---|---|
|  | 173.89 µs | 173.53 µs | 14.68 µs | Upfront buffer allocation |
|  | 193.21 µs | 204.38 µs | 5.77 ms | Sequential fill 256 tokens |
|  | 21.84 µs | 22.45 µs | 1.37 ms | Batch write 64 tokens under 1 lock |
|  | 93.79 ns | 84.79 ns | 1.30 µs | Owned read ( Vec alloc) |
|  | **21.92 ns** | **21.91 ns** | **21.89 ns** | Zero-copy closure read (no alloc) |
|  | 94.86 ns | 84.44 ns | 1.28 µs | Owned range read 1 token |
|  | **21.89 ns** | **21.90 ns** | **21.88 ns** | Zero-copy closure range 1 token |
|  | **53.90 ns** | **55.46 ns** | **1.12 µs** | Caller-buffer copy range 1 token |
|  | 5.38 µs | 4.96 µs | 150.34 µs | Owned range read 64 tokens |
|  | **21.90 ns** | **21.92 ns** | **21.88 ns** | Zero-copy closure range 64 tokens |
|  | **7.04 µs** | **4.93 µs** | **150.93 µs** | Caller-buffer copy range 64 tokens |
|  | 137.78 µs | 141.83 µs | 32.54 ms | Owned range read 1024 tokens |
|  | **21.93 ns** | **21.91 ns** | **21.92 ns** | Zero-copy closure range 1024 tokens |
|  | 144.55 µs | 138.13 µs | **6.65 ms** | Caller-buffer copy range 1024 tokens |
|  | 2.90 ms | 3.18 ms | 107.08 ms | Owned decode step loop (256 steps) |
|  | **202.41 µs** | **197.61 µs** | **5.81 ms** | Zero-copy decode step loop (~15-18x speedup) |
|  | 25.38 µs | 25.77 µs | 1.35 ms | 4 reader threads looping owned range |
|  | **341.26 ns** | **340.75 ns** | **340.19 ns** | 4 reader threads zero-copy (~74x-3900x speedup) |
---
