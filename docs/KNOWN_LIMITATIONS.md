# Known Limitations

This document outlines the current architectural and operational limitations of Ghostlink.

## 1. RPC Security Model (Unencrypted ggml-rpc)

- **Default Unencrypted RPC**: By default, `ggml-rpc` tensor traffic between cluster nodes over `rpc_port` (default `50052`) is unencrypted.
- **Trust Domain**: Ghostlink is designed for a trusted Local Area Network (LAN). Do not expose `ggml-rpc` ports to the public internet without SSH tunneling or VPN encapsulation.

## 2. Large Model Offload Performance (30B+ Split)

- **Inter-node Bandwidth & Latency Impact**: Splitting 30B-class models (e.g. Qwen3-Coder-30B) across consumer-grade LAN nodes (e.g., integrated GPUs / Wi-Fi links) typically achieves ~1.5–2.5 tok/s decode performance.
- **Trade-off**: The primary benefit of multi-node splitting on consumer hardware is enabling models to fit in collective RAM/VRAM without hard OOM errors, rather than high decode speed.

## 3. Prebuilt Installers & Binary Scope

- **x86_64 Binaries**: The standard installer scripts (`scripts/install.sh` and `scripts/install.ps1`) currently target prebuilt `x86_64` host architectures for release binaries. (Note: CI cross-compiles `aarch64` Linux/macOS binaries, but installer integration is pending PR 4).
- **Binary Distribution Scope**: Prebuilt release archives contain the standalone `ghost-link` CLI binary. They do not package the Node.js/Vite React frontend (`ghostlink_gui_modern/`) or the Go gateway (`control-plane/`). To run the full web GUI or gateway, source checkout and local runtime setup (Node.js/npm, Go) are required.

## 4. LAN-Trust Security Model

- **Peer Admission**: HMAC shared secret peer authentication (`rpc_shared_secret`) secures node registration, but transport payloads rely on network-level trust.
- **Access Control**: Role-based access control (RBAC) gates API routes (`/v1/*`, `/api/*`), but network isolation remains essential for cluster node boundaries.
