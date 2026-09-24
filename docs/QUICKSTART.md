# Quickstart

Ghostlink is a self-hosted LAN orchestrator that turns machines on your network into a single inference cluster.

## The Happy Path

**Discover peers → Split GGUF weights via `ggml-rpc` → Serve `/v1/chat/completions`**

### Step 1: Start Ghostlink Coordinator

Start the API and orchestrator on machine 1:
```bash
ghost-link serve 127.0.0.1 8003
```

### Step 2: Connect LAN Worker Node(s)

Start a worker on machine 2 pointing to the coordinator:
```bash
ghost-link cluster join --coordinator 192.168.1.100:8003 --rpc-port 50052
```
Ghostlink automatically discovers the worker over LAN and registers its compute capacity (VRAM/RAM).

### Step 3: Serve OpenAI-Compatible Chat Inference

Send inference requests to the coordinator's OpenAI-compatible API endpoint:
```bash
curl http://127.0.0.1:8003/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen2.5-1.5b-instruct",
    "messages": [{"role": "user", "content": "Hello LAN cluster!"}],
    "stream": true
  }'
```
Ghostlink offloads model tensor splits across connected nodes automatically via `ggml-rpc`.

---

## 2-Node Reproduce Recipe (Real LAN Hardware)

To reproduce the real 2-node distributed inference benchmark from [BENCHMARKS.md](BENCHMARKS.md):

1. **Hardware Setup**:
   - Coordinator: Windows 11 / Linux host (e.g. laptop with integrated GPU).
   - Worker: Linux host (e.g. Mini PC or second node).
2. **Start Contributor on Worker Node**:
   ```bash
   ghost-link worker start --rpc-port 50052
   ```
3. **Trigger Discovery & Load Model on Coordinator**:
   ```bash
   # Discover workers on local LAN subnet
   curl http://127.0.0.1:8003/api/workers/discover

   # Post chat request with offload enabled
   curl http://127.0.0.1:8003/v1/chat/completions \
     -H "Content-Type: application/json" \
     -d '{"model": "qwen2.5-1.5b-instruct-q4_k_m.gguf", "messages": [{"role": "user", "content": "Benchmark test"}]}'
   ```
4. **Measured Benchmark Results (see `docs/BENCHMARKS.md`)**:
   - **Qwen2.5 1.5B (1.04 GB)**: ~53.57 tok/s (2-node LAN split).
   - **Qwen3-Coder 30B (13.58 GB)**: ~1.47–2.52 tok/s (2-node LAN split; enables running models that otherwise hard-OOM on single host).

---

## Experimental / Research Features

*Note: Features like `ghost-link flow`, SPSC ring-buffer channels, and AF_XDP kernel-bypass transport are experimental research components for low-latency framing and pipeline modeling. Synthetic transport numbers (e.g., in-memory ring buffer ns/op or 340k tok/s 64-byte payload throughput) measure transport framing overhead only and do **not** reflect real LLM token inference speed.*

