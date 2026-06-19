//! Per-model context/output budgets, used to size compaction and the request's
//! `max_tokens`.
//!
//! This is intentionally **data**, not a provider call: a small lookup keyed by
//! a model-id substring with a conservative default. Counts are estimates for
//! budget planning (compaction trigger, output cap), not a protocol boundary, so
//! a slightly-off entry only changes when we compact — never correctness.

use serde::{Deserialize, Serialize};

/// Context-window and output budgets for one model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Total tokens the model accepts (prompt + response) in one request.
    pub context_window: usize,
    /// Maximum tokens the model will generate in one response.
    pub max_output_tokens: u32,
}

impl Default for ModelCapabilities {
    /// A conservative floor for an unrecognized model: a 128k window with an 8k
    /// output cap. Picking small keeps us safely inside any real model's limits.
    fn default() -> Self {
        Self {
            context_window: 128_000,
            max_output_tokens: 8_192,
        }
    }
}

/// `(model-id substring, capabilities)` table, scanned in order; the first
/// substring contained in the (lowercased) model id wins. Ordered most- to
/// least-specific so e.g. `claude-3-5-haiku` is matched before a bare `claude`.
const TABLE: &[(&str, ModelCapabilities)] = &[
    // Extended-context Claude variants MUST precede the bare-family entries
    // below: the scan is in-order substring matching, so a bare `opus`/`claude`
    // needle would otherwise match the 1M-context Opus 4.8 at 200k and trigger
    // token-based compaction far too early. The needle is anchored on the `[1m]`
    // suffix bracket to avoid a mildly collision-prone bare `1m`. Output budget
    // is the Opus family's 32k.
    (
        "opus-4-8[1m]",
        ModelCapabilities {
            context_window: 1_000_000,
            max_output_tokens: 32_000,
        },
    ),
    // Anthropic Claude: 200k window. Sonnet/Opus generate up to 64k; Haiku 8k.
    (
        "haiku",
        ModelCapabilities {
            context_window: 200_000,
            max_output_tokens: 8_192,
        },
    ),
    (
        "sonnet",
        ModelCapabilities {
            context_window: 200_000,
            max_output_tokens: 64_000,
        },
    ),
    (
        "opus",
        ModelCapabilities {
            context_window: 200_000,
            max_output_tokens: 32_000,
        },
    ),
    (
        "claude",
        ModelCapabilities {
            context_window: 200_000,
            max_output_tokens: 32_000,
        },
    ),
    // OpenAI GPT-5 / o-series: ~400k window, large output budget.
    (
        "gpt-5",
        ModelCapabilities {
            context_window: 400_000,
            max_output_tokens: 128_000,
        },
    ),
    (
        "gpt-4.1",
        ModelCapabilities {
            context_window: 1_000_000,
            max_output_tokens: 32_768,
        },
    ),
    (
        "gpt-4o",
        ModelCapabilities {
            context_window: 128_000,
            max_output_tokens: 16_384,
        },
    ),
    // xAI Grok: large windows; conservative output cap.
    (
        "grok",
        ModelCapabilities {
            context_window: 256_000,
            max_output_tokens: 32_768,
        },
    ),
];

impl ModelCapabilities {
    /// Look up capabilities for `model`, falling back to [`Default`] when no
    /// substring matches. The match is case-insensitive on the model id.
    #[must_use]
    pub fn for_model(model: &str) -> Self {
        let lower = model.to_ascii_lowercase();
        TABLE
            .iter()
            .find(|(needle, _)| lower.contains(needle))
            .map(|(_, caps)| *caps)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_uses_conservative_default() {
        let caps = ModelCapabilities::for_model("some-future-model");
        assert_eq!(caps, ModelCapabilities::default());
        assert_eq!(caps.context_window, 128_000);
        assert_eq!(caps.max_output_tokens, 8_192);
    }

    #[test]
    fn claude_family_matches_by_substring() {
        assert_eq!(
            ModelCapabilities::for_model("claude-sonnet-4-6").context_window,
            200_000
        );
        // Haiku is more specific than the bare `claude` entry and wins.
        assert_eq!(
            ModelCapabilities::for_model("claude-3-5-haiku-latest").max_output_tokens,
            8_192
        );
        // A bare claude id still resolves (the catch-all claude entry).
        assert_eq!(
            ModelCapabilities::for_model("claude-x").context_window,
            200_000
        );
    }

    #[test]
    fn extended_context_opus_reports_one_million_window() {
        // The 1M-context Opus 4.8 must NOT fall through to the bare `opus`/`claude`
        // 200k entry (which would compact far too early). The `[1m]`-anchored entry
        // wins the in-order scan.
        let caps = ModelCapabilities::for_model("claude-opus-4-8[1m]");
        assert_eq!(caps.context_window, 1_000_000);
        // Output budget stays the Opus family's 32k.
        assert_eq!(caps.max_output_tokens, 32_000);
        // The non-1M Opus id still resolves to the standard 200k window.
        assert_eq!(
            ModelCapabilities::for_model("claude-opus-4-8").context_window,
            200_000
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(
            ModelCapabilities::for_model("GPT-5-MINI").context_window,
            400_000
        );
    }

    #[test]
    fn grok_matches() {
        assert_eq!(
            ModelCapabilities::for_model("grok-4.3").context_window,
            256_000
        );
    }
}
