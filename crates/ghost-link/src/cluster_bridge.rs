//! Cluster bridge — exposes heterogeneous node capabilities to the task agent runtime.
//!
//! The task runtime needs to reason about real node hardware (CPU/CUDA/ROCm/Vulkan/Metal/NPU,
//! VRAM, RAM, network) when placing agents and choosing models. This module provides a
//! read-only view over `ghostlink-core`'s `ClusterState` so the agent loop can see
//! what nodes exist and what they can do.
//!
//! When no cluster is available (single-node mode), `query_cluster_nodes` returns
//! an empty vec and the runtime behaves exactly as before.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A read-only snapshot of one node's capabilities for agent planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapability {
    pub node_id: String,
    pub name: String,
    pub status: String, // "active" | "degraded" | "failed"
    pub vram_gb: f32,
    pub system_memory_gb: f32,
    /// "cuda" | "rocm" | "vulkan" | "metal" | "npu" | "cpu"
    pub compute_capability: String,
    pub gpu_name: Option<String>,
    pub available_vram_gb: f32,
    pub avg_latency_us: f32,
    /// (current_layers, total_layers) if this node is streaming
    pub streaming_layers: Option<(usize, usize)>,
}

/// Query the live cluster for active nodes.
///
/// Returns an empty vec if the cluster is unavailable (single-node mode)
/// or if no nodes are currently active. This is the common case for a
/// single-machine deployment — the task runtime handles it gracefully.
pub async fn query_cluster_nodes(
    cluster: &Arc<ghostlink_core::cluster::ClusterState>,
) -> Vec<NodeCapability> {
    cluster
        .active_nodes()
        .into_iter()
        .map(|nm| NodeCapability {
            node_id: nm.name.clone(),
            name: nm.name.clone(),
            status: match nm.status {
                ghostlink_core::cluster::NodeStatus::Active => "active",
                ghostlink_core::cluster::NodeStatus::Degraded => "degraded",
                ghostlink_core::cluster::NodeStatus::Failed => "failed",
            }
            .to_string(),
            vram_gb: nm.total_vram_gb,
            system_memory_gb: nm.system_memory_gb,
            compute_capability: nm.compute_capability.clone(),
            gpu_name: nm.gpu_name.clone(),
            available_vram_gb: nm.available_vram_gb,
            avg_latency_us: nm.avg_latency_us,
            streaming_layers: nm.streaming_layers,
        })
        .collect()
}

/// Format cluster topology as a system-prompt prefix for the model.
///
/// This is injected into the agent's system prompt so the model can make
/// placement decisions (e.g., "heavy coding on GPU nodes, light verification
/// on CPU, vision on NPU if available").
pub fn format_cluster_prompt(nodes: &[NodeCapability]) -> String {
    if nodes.is_empty() {
        return String::new();
    }

    let mut prompt = String::from("\n\nCluster topology for this run:\n");
    for node in nodes {
        prompt.push_str(&format!(
            "- {}: {}, {}GB VRAM ({}GB free), {}GB RAM",
            node.name,
            node.compute_capability,
            node.vram_gb,
            node.available_vram_gb,
            node.system_memory_gb,
        ));
        if let Some(ref gpu) = node.gpu_name {
            prompt.push_str(&format!(" ({})", gpu));
        }
        if let Some((cur, total)) = node.streaming_layers {
            prompt.push_str(&format!(" [streaming {}/{} layers]", cur, total));
        }
        prompt.push('\n');
    }
    prompt.push_str(
        "\nPlacement guidance: heavy coding tasks → GPU nodes; light verification → CPU; \
         vision → NPU if available. Use the `node` field in run_command to target a specific node.\n",
    );
    prompt
}

/// Choose the best node for a task based on its description.
///
/// Returns `None` if no nodes are available (single-node mode) or if every
/// node is non-active. The `task_hint` is a short description like "heavy
/// coding", "light verification", or "vision".
///
/// Status is filtered by the caller (`query_cluster_nodes` already returns
/// only active nodes via `ClusterState::active_nodes`), but the filter is
/// kept here so the function is correct on any input, including a snapshot
/// built by hand in a test.
pub fn choose_node_for_task<'a>(
    nodes: &'a [NodeCapability],
    task_hint: &str,
) -> Option<&'a NodeCapability> {
    let active: Vec<&NodeCapability> = nodes.iter().filter(|n| n.status == "active").collect();

    if active.is_empty() {
        return None;
    }

    let hint_lower = task_hint.to_lowercase();

    // Vision tasks → NPU if available
    if hint_lower.contains("vision") || hint_lower.contains("image") {
        if let Some(npu) = active.iter().find(|n| n.compute_capability == "npu") {
            return Some(npu);
        }
    }

    // Heavy coding → GPU with most available VRAM. Falls through to the
    // default when the cluster is CPU-only rather than returning None: a
    // single-node CPU deployment still has *a* node to run on, and the
    // previous `return` here made "heavy" tasks unplaceable exactly when
    // there was no accelerator to choose between.
    if hint_lower.contains("heavy")
        || hint_lower.contains("coding")
        || hint_lower.contains("model")
        || hint_lower.contains("training")
    {
        if let Some(gpu) = active
            .iter()
            .filter(|n| n.compute_capability != "cpu")
            .max_by(|a, b| {
                a.available_vram_gb
                    .partial_cmp(&b.available_vram_gb)
                    .unwrap()
            })
        {
            return Some(gpu);
        }
    }

    // Light verification → CPU node (don't waste GPU). Falls through to the
    // default when there is no CPU-only node.
    if hint_lower.contains("light")
        || hint_lower.contains("verification")
        || hint_lower.contains("test")
        || hint_lower.contains("lint")
    {
        if let Some(cpu) = active.iter().find(|n| n.compute_capability == "cpu") {
            return Some(cpu);
        }
    }

    // Default: first active node
    active.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, compute: &str, vram: f32, avail: f32) -> NodeCapability {
        NodeCapability {
            node_id: id.into(),
            name: id.into(),
            status: "active".into(),
            vram_gb: vram,
            system_memory_gb: 32.0,
            compute_capability: compute.into(),
            gpu_name: Some(format!("GPU-{}", id)),
            available_vram_gb: avail,
            avg_latency_us: 100.0,
            streaming_layers: None,
        }
    }

    #[test]
    fn empty_cluster_returns_none() {
        assert!(choose_node_for_task(&[], "coding").is_none());
    }

    #[test]
    fn heavy_coding_prefers_gpu() {
        let nodes = vec![
            node("cpu1", "cpu", 0.0, 0.0),
            node("gpu1", "cuda", 24.0, 12.0),
            node("gpu2", "cuda", 48.0, 20.0),
        ];
        let chosen = choose_node_for_task(&nodes, "heavy coding").unwrap();
        assert_eq!(chosen.node_id, "gpu2");
    }

    /// A CPU-only cluster still has a node to run on. The heavy branch used to
    /// `return` the `max_by` result directly, so with every node classified
    /// "cpu" the filter was empty and the function returned `None` — exactly
    /// the single-node deployment the module claims to support.
    #[test]
    fn heavy_coding_on_cpu_only_cluster_falls_back_to_first_node() {
        let nodes = vec![node("cpu1", "cpu", 0.0, 0.0), node("cpu2", "cpu", 0.0, 0.0)];
        let chosen = choose_node_for_task(&nodes, "heavy coding").unwrap();
        assert_eq!(chosen.node_id, "cpu1");
    }

    /// Non-active nodes are never chosen, even as the fallback.
    #[test]
    fn non_active_nodes_are_never_chosen() {
        let mut failed = node("cpu1", "cpu", 0.0, 0.0);
        failed.status = "failed".into();
        let nodes = vec![failed, node("cpu2", "cpu", 0.0, 0.0)];
        let chosen = choose_node_for_task(&nodes, "light verification").unwrap();
        assert_eq!(chosen.node_id, "cpu2");
    }

    #[test]
    fn light_verification_prefers_cpu() {
        let nodes = vec![
            node("cpu1", "cpu", 0.0, 0.0),
            node("gpu1", "cuda", 24.0, 12.0),
        ];
        let chosen = choose_node_for_task(&nodes, "light verification").unwrap();
        assert_eq!(chosen.node_id, "cpu1");
    }

    #[test]
    fn vision_prefers_npu() {
        let nodes = vec![
            node("cpu1", "cpu", 0.0, 0.0),
            node("gpu1", "cuda", 24.0, 12.0),
            node("npu1", "npu", 16.0, 8.0),
        ];
        let chosen = choose_node_for_task(&nodes, "vision task").unwrap();
        assert_eq!(chosen.node_id, "npu1");
    }

    #[test]
    fn format_cluster_prompt_empty() {
        assert_eq!(format_cluster_prompt(&[]), "");
    }

    /// `query_cluster_nodes` is the only function that touches live cluster
    /// state, and it had no test. An empty `ClusterState` must yield an empty
    /// vec — this is the single-node path every local deployment takes, and a
    /// panic or an error here would surface as a failed task spawn.
    #[tokio::test]
    async fn query_cluster_nodes_on_empty_cluster_returns_empty() {
        let cluster = Arc::new(ghostlink_core::cluster::ClusterState::new());
        let nodes = query_cluster_nodes(&cluster).await;
        assert!(nodes.is_empty(), "empty cluster must yield no nodes");
        assert_eq!(format_cluster_prompt(&nodes), "");
    }

    /// A registered node must appear with its metrics mapped through, and a
    /// node marked Failed must not (that is `active_nodes`' contract, and the
    /// agent loop depends on never being offered a dead node).
    #[tokio::test]
    async fn query_cluster_nodes_maps_registered_nodes_and_excludes_failed() {
        let cluster = Arc::new(ghostlink_core::cluster::ClusterState::new());

        let live = ghostlink_core::protocol::NodeResources::new(
            "gpu-a",
            24.0,
            64.0,
            "cuda",
            Some("RTX 4090".to_string()),
        );
        cluster.register(live);

        let dead = ghostlink_core::protocol::NodeResources::new(
            "gpu-b",
            16.0,
            32.0,
            "cuda",
            Some("Dead GPU".to_string()),
        );
        cluster.register(dead);
        cluster.mark_failed("gpu-b");

        let nodes = query_cluster_nodes(&cluster).await;
        assert_eq!(nodes.len(), 1, "only the active node is returned");
        let n = &nodes[0];
        assert_eq!(n.name, "gpu-a");
        assert_eq!(n.node_id, "gpu-a");
        assert_eq!(n.status, "active");
        assert_eq!(n.compute_capability, "cuda");
        assert_eq!(n.gpu_name.as_deref(), Some("RTX 4090"));
        assert_eq!(n.vram_gb, 24.0);
        assert_eq!(n.system_memory_gb, 64.0);
        assert_eq!(n.streaming_layers, None);
    }

    /// The prompt is what the model actually reads, so its content is the
    /// contract: a node line plus the placement guidance that tells the model
    /// how to use the `node` field.
    #[test]
    fn format_cluster_prompt_lists_nodes() {
        let nodes = vec![node("gpu1", "cuda", 24.0, 12.0)];
        let prompt = format_cluster_prompt(&nodes);
        assert!(prompt.contains("gpu1"));
        assert!(prompt.contains("cuda"));
        assert!(prompt.contains("24"));
    }
}
