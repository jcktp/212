//! A small curated list of models known to work well with llama.cpp, used for
//! first-run recommendations. Sizes are fetched live from Hugging Face before
//! any download, and every download needs explicit approval.

#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub spec: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    /// Approximate download size in GiB (shown before live metadata arrives).
    pub approx_gib: f32,
    /// Minimum total memory (GiB) for a comfortable experience at 8k context.
    pub min_mem_gib: u32,
    pub tools: bool,
}

pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        spec: "Qwen/Qwen3-0.6B-GGUF:Q8_0",
        title: "Qwen3 0.6B",
        blurb: "tiny and fast; good for testing, weak at complex tasks",
        approx_gib: 0.6,
        min_mem_gib: 4,
        tools: true,
    },
    CatalogEntry {
        spec: "Qwen/Qwen3-1.7B-GGUF:Q8_0",
        title: "Qwen3 1.7B",
        blurb: "small general assistant",
        approx_gib: 1.7,
        min_mem_gib: 6,
        tools: true,
    },
    CatalogEntry {
        spec: "Qwen/Qwen3-4B-GGUF:Q4_K_M",
        title: "Qwen3 4B",
        blurb: "solid all-rounder for 8–16 GB machines",
        approx_gib: 2.3,
        min_mem_gib: 8,
        tools: true,
    },
    CatalogEntry {
        spec: "ggml-org/gemma-3-4b-it-GGUF:Q4_K_M",
        title: "Gemma 3 4B",
        blurb: "strong writing and multilingual chat",
        approx_gib: 2.3,
        min_mem_gib: 8,
        tools: false,
    },
    CatalogEntry {
        spec: "Qwen/Qwen3-8B-GGUF:Q4_K_M",
        title: "Qwen3 8B",
        blurb: "better reasoning and coding; 16 GB+",
        approx_gib: 4.7,
        min_mem_gib: 14,
        tools: true,
    },
    CatalogEntry {
        spec: "Qwen/Qwen3-14B-GGUF:Q4_K_M",
        title: "Qwen3 14B",
        blurb: "capable coding agent; 24 GB+",
        approx_gib: 8.4,
        min_mem_gib: 22,
        tools: true,
    },
    CatalogEntry {
        spec: "ggml-org/gpt-oss-20b-GGUF:MXFP4",
        title: "gpt-oss 20B",
        blurb: "MoE reasoning model with tool use; 24 GB+",
        approx_gib: 11.3,
        min_mem_gib: 24,
        tools: true,
    },
];

/// Entries that should run comfortably with `total_mem` bytes, best first.
pub fn recommend(total_mem: u64) -> Vec<&'static CatalogEntry> {
    let gib = (total_mem / (1 << 30)) as u32;
    let mut v: Vec<_> = CATALOG.iter().filter(|e| e.min_mem_gib <= gib).collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.min_mem_gib));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recommends_by_memory() {
        let r = recommend(16 << 30);
        assert_eq!(r[0].title, "Qwen3 8B");
        assert!(r.iter().all(|e| e.min_mem_gib <= 16));
        assert!(recommend(2 << 30).is_empty());
    }
}
