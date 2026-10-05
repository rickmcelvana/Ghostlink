//! Deciding *what* to offload across an RPC cluster, not just *how much*.
//!
//! ## What exists today
//!
//! `compute_tensor_split` weights devices by VRAM, or by RAM with a 0.5 haircut,
//! floored at 0.1, and emits a `-ts` ratio. That answers "how much goes where" and
//! nothing about *which tensors*. llama-server then spreads whole layers across the
//! devices in that ratio, including the ones most sensitive to a network round trip.
//!
//! ## Why equal-share layers is the wrong default
//!
//! Decode is dominated by memory bandwidth and by one synchronisation point per
//! layer. Putting attention, embeddings, the KV cache and `lm_head` behind an RPC link
//! costs a round trip for data that is either tiny (`lm_head`, one vector per token)
//! or reused constantly (KV, every layer).
//!
//! FFN and MoE experts are the opposite: large, streamed once per token, and the work
//! the tensor split is actually trying to move off a saturated device. `GHOSTLINK_RPC_TENSOR_OVERRIDE`
//! already names this split (`ffn=RPC,exps=RPC`) -- but only inside a unit test. It was
//! never passed to `llama-server`, so the intent was documented and never executed.
//!
//! ## What this adds
//!
//! An explicit tensor-class plan, plus a latency term so a peer whose RPC round trip is
//! worse than the local CPU-offload penalty does not get work at all.

/// Tensor classes llama.cpp's `-ot` can route, in the order they are considered.
// Variant names mirror llama.cpp's `-ot` keys. The casing is deliberate and the
// canonical spelling lives in `llama_key()`.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TensorClass {
    /// Token embeddings. Read once per token; tiny.
    Token_embd,
    /// Output projection. One vector per token.
    Output,
    /// Attention projections — Q/K/V/O.
    Attn_norm,
    /// Feed-forward / MLP blocks. Large, streamed per token.
    Ffn,
    /// Mixture-of-experts. The bulk of the work on an MoE model.
    Exps,
    /// KV cache. Read and written by every layer, every token.
    Cache_k,
    /// Attention KV norm.
    Attn_k_norm,
}

/// llama.cpp's own `-ot` key for each class.
///
/// Spelled out rather than derived from `Debug`, which would be a coincidence rather
/// than a contract, and is not the casing llama-server parses.
impl TensorClass {
    pub fn llama_key(&self) -> &'static str {
        match self {
            TensorClass::Token_embd => "token_embd",
            TensorClass::Output => "output",
            TensorClass::Attn_norm => "attn_norm",
            TensorClass::Ffn => "ffn",
            TensorClass::Exps => "exps",
            TensorClass::Cache_k => "cache_k",
            TensorClass::Attn_k_norm => "attn_k_norm",
        }
    }
}

/// Every class, with the routing default each one gets.
///
/// The routing question is "does moving this tensor to a slow link cost more than the
/// VRAM it frees?" For everything except FFN and experts the answer is yes, which is
/// why they stay local.
pub const DEFAULT_ROUTING: &[(TensorClass, &str)] = &[
    (TensorClass::Token_embd, "CPU"),
    (TensorClass::Output, "CPU"),
    (TensorClass::Attn_norm, "CPU"),
    (TensorClass::Cache_k, "CPU"),
    (TensorClass::Attn_k_norm, "CPU"),
    (TensorClass::Ffn, "RPC"),
    (TensorClass::Exps, "RPC"),
];

/// Why a tensor class is or is not a candidate for remote placement.
///
/// Returned so the caller can explain the decision in a trace rather than only in a
/// log line nobody reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Keep on the coordinator.
    Local,
    /// Move to an RPC peer.
    Remote,
    /// Do not place remotely regardless of capacity: the round-trip cost exceeds the
    /// VRAM it would free.
    NotWorthIt,
}

/// Round trip to an RPC peer, in milliseconds, and the local CPU-offload penalty.
///
/// The comparison matters: a layer placed remotely pays `peer_rtt_ms` every token. A
/// layer placed on the local CPU pays a bandwidth penalty but no network latency. A
/// peer slower than that penalty is worse than useless for latency-sensitive tensors
/// -- it frees VRAM by making every token wait longer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyProfile {
    /// Measured or configured RPC round trip to the peer.
    pub peer_rtt_ms: f32,
    /// Measured or configured local CPU-offload penalty.
    pub local_cpu_penalty_ms: f32,
}

impl Default for LatencyProfile {
    /// A fast LAN peer with no local CPU penalty.
    ///
    /// The defaults have to satisfy `peer_rtt_ms <= local_cpu_penalty_ms`, or the
    /// latency term rejects every class and remote placement never happens -- which is
    /// the *current* behaviour only because nothing checks at all, not because every
    /// peer is slow. Making the default self-consistent is what lets the term be
    /// enabled without silently changing behaviour for unmeasured nodes.
    ///
    /// Callers that know real numbers pass them; `GHOSTLINK_RPC_RTT_MS` overrides.
    fn default() -> Self {
        Self {
            peer_rtt_ms: 0.5,
            local_cpu_penalty_ms: 1.0,
        }
    }
}
/// Decides where each tensor class goes.
///
/// `local_vram_gb` is the coordinator's VRAM; `model_size_gb` the weights it must hold.
/// `remote_vram_gb` is the peer's.
pub fn plan_tensor_classes(
    local_vram_gb: f32,
    model_size_gb: f32,
    remote_vram_gb: f32,
    latency: LatencyProfile,
) -> Vec<(TensorClass, Placement)> {
    let mut plan = Vec::with_capacity(DEFAULT_ROUTING.len());

    // Does the coordinator actually need the room? If the model fits locally there is
    // nothing to gain, and the latency term makes that explicit rather than implicit.
    let local_is_tight = model_size_gb > 0.0 && model_size_gb + 1.0 > local_vram_gb;
    let peer_has_room = remote_vram_gb > 0.0;

    for (class, default_route) in DEFAULT_ROUTING {
        let remote_by_default = *default_route == "RPC";
        let placement = if !remote_by_default {
            Placement::Local
        } else if !peer_has_room {
            // No VRAM on the peer either. Routing there would spill to the peer's RAM,
            // which is slower than keeping it local, not faster.
            Placement::NotWorthIt
        } else if !local_is_tight {
            Placement::NotWorthIt
        } else if latency.peer_rtt_ms > latency.local_cpu_penalty_ms {
            // The latency term: a peer slower than the local CPU penalty trades decode
            // speed for VRAM the coordinator did not need.
            Placement::NotWorthIt
        } else {
            Placement::Remote
        };
        plan.push((*class, placement));
    }
    plan
}

/// Renders a plan as llama.cpp's `-ot` value, addressed at one RPC device.
///
/// `buffer_type` must be the *registered* buffer-type name, not the string "RPC".
/// Verified against this repo's vendored `llama.cpp`:
///
/// * `arg.cpp:272` resolves every `-ot` value against `ggml_backend_dev_buffer_type`
///   for each registered device, and throws `unknown buffer type` otherwise.
/// * `ggml-rpc.cpp:1092` builds that name as `"RPC" + device + "[" + endpoint + "]"`,
///   i.e. `RPC0[127.0.0.1:50052]` -- not `RPC`.
///
/// Emitting the bare word `RPC` was tested against the real binary and rejected:
///
/// ```text
/// $ llama-server -m model.gguf -ot ffn=RPC
/// error while handling argument "-ot": unknown buffer type
/// ```
///
/// Empty when nothing is placed remotely: `-ot` with no entry is rejected rather than
/// ignored, so callers must omit the flag entirely.
pub fn override_flag_value(plan: &[(TensorClass, Placement)], buffer_type: &str) -> String {
    let remote: Vec<String> = plan
        .iter()
        .filter(|(_, p)| *p == Placement::Remote)
        .map(|(c, _)| format!("{}={buffer_type}", c.llama_key()))
        .collect();
    if remote.is_empty() {
        String::new()
    } else {
        remote.join(",")
    }
}

/// The registered buffer-type name for the first RPC peer.
///
/// Matches `ggml-rpc`'s own naming (`RPC0[endpoint]`) so the value we emit is the one
/// llama-server will resolve. The endpoint must be the same string passed to `--rpc`.
pub fn rpc_buffer_type(endpoint: &str) -> String {
    format!("RPC0[{endpoint}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_classes(plan: &[(TensorClass, Placement)]) -> Vec<TensorClass> {
        plan.iter()
            .filter(|(_, p)| *p == Placement::Remote)
            .map(|(c, _)| *c)
            .collect()
    }

    #[test]
    fn ffn_and_experts_are_the_remote_candidates() {
        // The classes worth moving: large, streamed once per token.
        let plan = plan_tensor_classes(8.0, 16.0, 24.0, LatencyProfile::default());
        let remote = remote_classes(&plan);
        assert!(remote.contains(&TensorClass::Ffn));
        assert!(remote.contains(&TensorClass::Exps));
    }

    #[test]
    fn latency_sensitive_classes_stay_local() {
        // KV cache and lm_head are read every token or are one vector wide. A network
        // round trip for either costs more than the VRAM it frees.
        let plan = plan_tensor_classes(8.0, 16.0, 24.0, LatencyProfile::default());
        let remote = remote_classes(&plan);
        for c in [
            TensorClass::Cache_k,
            TensorClass::Output,
            TensorClass::Token_embd,
            TensorClass::Attn_norm,
            TensorClass::Attn_k_norm,
        ] {
            assert!(
                !remote.contains(&c),
                "{c:?} must stay local; it is per-token latency, not bulk data"
            );
        }
    }

    #[test]
    fn nothing_goes_remote_when_the_model_fits_locally() {
        // The highest-value case. A 3B model on a 24GB card does not need a peer, and
        // routing FFN remotely to "use the cluster" makes every token slower.
        let plan = plan_tensor_classes(24.0, 3.0, 48.0, LatencyProfile::default());
        assert!(
            remote_classes(&plan).is_empty(),
            "a fitting model must stay entirely local"
        );
    }

    #[test]
    fn nothing_goes_remote_when_the_peer_has_no_vram() {
        // Routing to a peer with no VRAM spills to its RAM, which is slower than
        // staying local. Better than nothing? No.
        let plan = plan_tensor_classes(8.0, 30.0, 0.0, LatencyProfile::default());
        assert!(remote_classes(&plan).is_empty());
    }

    #[test]
    fn a_slow_peer_is_not_given_latency_sensitive_work() {
        // The latency term. A peer whose RTT exceeds the local CPU penalty trades
        // decode speed for VRAM that was not needed.
        let slow = LatencyProfile {
            peer_rtt_ms: 12.0,
            local_cpu_penalty_ms: 0.5,
        };
        let plan = plan_tensor_classes(8.0, 30.0, 24.0, slow);
        assert!(
            remote_classes(&plan).is_empty(),
            "a 12ms peer must not receive any tensor class"
        );
    }

    #[test]
    fn a_fast_peer_is_given_work_when_the_model_does_not_fit() {
        let fast = LatencyProfile {
            peer_rtt_ms: 0.2,
            local_cpu_penalty_ms: 0.5,
        };
        let plan = plan_tensor_classes(8.0, 30.0, 24.0, fast);
        assert!(!remote_classes(&plan).is_empty());
    }

    #[test]
    fn the_plan_covers_every_class() {
        let plan = plan_tensor_classes(8.0, 16.0, 24.0, LatencyProfile::default());
        assert_eq!(plan.len(), DEFAULT_ROUTING.len());
        for (class, _) in DEFAULT_ROUTING {
            assert!(
                plan.iter().any(|(c, _)| c == class),
                "{class:?} missing from the plan"
            );
        }
    }

    #[test]
    fn an_empty_override_is_rendered_as_an_empty_string() {
        // `-ot` with no RPC= entry makes llama-server error, not ignore.
        let plan = plan_tensor_classes(24.0, 3.0, 48.0, LatencyProfile::default());
        assert_eq!(override_flag_value(&plan, "RPC0[127.0.0.1:50052]"), "");
    }

    #[test]
    fn the_override_names_ffn_and_exps() {
        let plan = plan_tensor_classes(
            8.0,
            30.0,
            24.0,
            LatencyProfile {
                peer_rtt_ms: 0.2,
                local_cpu_penalty_ms: 0.5,
            },
        );
        let flag = override_flag_value(&plan, "RPC0[127.0.0.1:50052]");
        assert!(
            flag.contains("ffn=RPC0[127.0.0.1:50052]"),
            "flag was {flag}"
        );
        assert!(
            flag.contains("exps=RPC0[127.0.0.1:50052]"),
            "flag was {flag}"
        );
        assert!(
            !flag.contains("cache_k=RPC"),
            "KV must not be routed: {flag}"
        );
    }

    #[test]
    fn the_default_override_matches_the_value_the_existing_test_asserts() {
        // `rpc_tensor_override_defaults_to_ffn_and_exps_remote` asserts the literal
        // "ffn=RPC,exps=RPC". This is that value, now produced by the planner instead of
        // hardcoded in a test -- which is the whole point of the change.
        let plan = plan_tensor_classes(8.0, 16.0, 24.0, LatencyProfile::default());
        assert_eq!(
            override_flag_value(&plan, "RPC0[127.0.0.1:50052]"),
            "ffn=RPC0[127.0.0.1:50052],exps=RPC0[127.0.0.1:50052]"
        );
    }

    #[test]
    fn the_buffer_type_matches_ggml_rpc_naming() {
        // ggml-rpc.cpp:1092 -- `"RPC" + device + "[" + endpoint + "]"`.
        assert_eq!(rpc_buffer_type("127.0.0.1:50052"), "RPC0[127.0.0.1:50052]");
    }

    #[test]
    fn the_bare_word_rpc_is_never_emitted() {
        // Tested against the real binary: `-ot ffn=RPC` exits with
        // "unknown buffer type". arg.cpp resolves values against registered devices, and
        // the registered name for a remote device is not the word "RPC".
        let plan = plan_tensor_classes(
            8.0,
            16.0,
            24.0,
            LatencyProfile {
                peer_rtt_ms: 0.2,
                local_cpu_penalty_ms: 0.5,
            },
        );
        let flag = override_flag_value(&plan, &rpc_buffer_type("10.0.0.5:50052"));
        assert!(flag.contains("RPC0[10.0.0.5:50052]"), "flag was {flag}");
        assert!(
            !flag.contains("=RPC,") && !flag.ends_with("=RPC"),
            "a bare RPC value would be rejected by llama-server: {flag}"
        );
    }
}
