//! Memory estimates and capability detection.
//!
//! File size alone is not a memory requirement: the KV cache grows with
//! context length and the runtime needs compute buffers. These estimates are
//! deliberately conservative and always presented as estimates.

use common::proto::Fit;

use crate::gguf::GgufInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryEstimate {
    pub weights: u64,
    pub kv_cache: u64,
    pub overhead: u64,
}

impl MemoryEstimate {
    pub fn total(&self) -> u64 {
        self.weights + self.kv_cache + self.overhead
    }
}

/// Estimate memory to run a model with `ctx` tokens of context and an f16 KV cache.
pub fn estimate(info: &GgufInfo, file_size: u64, ctx: u64) -> MemoryEstimate {
    let ctx = info.context_length().map_or(ctx, |max| ctx.min(max));
    let kv_cache = match (info.block_count(), info.head_count_kv(), info.embedding_length(), info.head_count()) {
        (Some(layers), Some(kv_heads), Some(embd), Some(heads)) if heads > 0 => {
            let k_dim = info.key_length().unwrap_or(embd / heads);
            let v_dim = info.value_length().unwrap_or(k_dim);
            layers * ctx * kv_heads * (k_dim + v_dim) * 2
        }
        // Unknown architecture: assume ~10% of weights per 8k tokens.
        _ => file_size / 10 * ctx.div_ceil(8192),
    };
    let overhead = 256 * 1024 * 1024 + file_size / 20;
    MemoryEstimate { weights: file_size, kv_cache, overhead }
}

/// Classify how a model of `need` bytes fits on a machine with `total` bytes of
/// (unified or GPU) memory.
pub fn fit(need: u64, total: u64) -> Fit {
    if total == 0 {
        return Fit::Unknown;
    }
    let ratio = need as f64 / total as f64;
    if ratio < 0.6 {
        Fit::Comfortable
    } else if ratio < 0.85 {
        Fit::Tight
    } else {
        Fit::TooLarge
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// The chat template declares tool support. Reliability still varies.
    pub tool_calling: bool,
    /// Emits separable reasoning (e.g. `<think>`).
    pub reasoning: bool,
    /// A vision projector is installed alongside.
    pub vision: bool,
    pub has_chat_template: bool,
}

impl Capabilities {
    pub fn detect(info: &GgufInfo, has_projector: bool) -> Self {
        let tpl = info.chat_template().unwrap_or("");
        Self {
            tool_calling: tpl.contains("tools") || tpl.contains("tool_call"),
            reasoning: tpl.contains("<think>") || tpl.contains("enable_thinking") || tpl.contains("reasoning"),
            vision: has_projector,
            has_chat_template: !tpl.is_empty(),
        }
    }

    pub fn labels(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.tool_calling {
            v.push("tools".into());
        }
        if self.reasoning {
            v.push("reasoning".into());
        }
        if self.vision {
            v.push("vision".into());
        }
        if !self.has_chat_template {
            v.push("no-chat-template".into());
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::testutil::{W, build};
    use std::io::Cursor;

    #[test]
    fn kv_cache_scales_with_context() {
        let info = GgufInfo::read(Cursor::new(build(&[
            ("general.architecture", W::Str("llama")),
            ("llama.context_length", W::U32(32768)),
            ("llama.block_count", W::U32(32)),
            ("llama.embedding_length", W::U32(4096)),
            ("llama.attention.head_count", W::U32(32)),
            ("llama.attention.head_count_kv", W::U32(8)),
        ])))
        .unwrap();
        let a = estimate(&info, 4 << 30, 4096);
        let b = estimate(&info, 4 << 30, 8192);
        // 32 layers * 4096 ctx * 8 heads * (128+128) * 2 bytes = 512 MiB
        assert_eq!(a.kv_cache, 512 * 1024 * 1024);
        assert_eq!(b.kv_cache, 2 * a.kv_cache);
        assert!(a.total() > 4 << 30);
    }

    #[test]
    fn context_is_capped_by_model_maximum() {
        let info = GgufInfo::read(Cursor::new(build(&[
            ("general.architecture", W::Str("llama")),
            ("llama.context_length", W::U32(2048)),
            ("llama.block_count", W::U32(1)),
            ("llama.embedding_length", W::U32(64)),
            ("llama.attention.head_count", W::U32(1)),
        ])))
        .unwrap();
        assert_eq!(estimate(&info, 0, 100_000).kv_cache, estimate(&info, 0, 2048).kv_cache);
    }

    #[test]
    fn fit_thresholds() {
        assert_eq!(fit(4, 16), Fit::Comfortable);
        assert_eq!(fit(12, 16), Fit::Tight);
        assert_eq!(fit(15, 16), Fit::TooLarge);
        assert_eq!(fit(1, 0), Fit::Unknown);
    }

    #[test]
    fn capabilities_from_template() {
        let info =
            GgufInfo::read(Cursor::new(build(&[("tokenizer.chat_template", W::Str("{% if tools %}<tool_call>{% endif %}<think>"))])))
                .unwrap();
        let c = Capabilities::detect(&info, false);
        assert!(c.tool_calling && c.reasoning && !c.vision && c.has_chat_template);
    }
}
