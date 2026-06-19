//! Approximate per-model metadata for the status bar's context-window % and
//! running cost. Ports `packages/tui/src/ui/models.ts`.
//!
//! Catalogs like this are inherently approximate and need upkeep; unknown models
//! simply show tokens with no % or cost. Prices are USD per million tokens
//! (input / output). The match is a first-wins ordered table of substrings (the
//! TS used regexes; the patterns here are simple enough to express as substring
//! checks against the lowercased id, which keeps the table dependency-free).

/// Approximate metadata for a model id.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelInfo {
    /// Context window in tokens (drives the status bar's `%`).
    pub context_window: u64,
    /// USD per million input tokens.
    pub input_per_mtok: f64,
    /// USD per million output tokens.
    pub output_per_mtok: f64,
}

impl ModelInfo {
    const fn new(context_window: u64, input_per_mtok: f64, output_per_mtok: f64) -> Self {
        Self {
            context_window,
            input_per_mtok,
            output_per_mtok,
        }
    }
}

/// A match rule: every substring in `needles` must be present (AND), in order to
/// match a model id. A single-element list is a plain "contains"; a 2-element
/// list expresses the TS `claude.*opus` form (both `claude` and `opus` present).
struct Rule {
    needles: &'static [&'static str],
    info: ModelInfo,
}

/// First matching rule wins (most specific first) — the order mirrors the TS
/// `TABLE`. Alternations in the TS regexes (`claude|sonnet`, `o3|gpt-4.1|gpt-4o`)
/// become several rules sharing one [`ModelInfo`].
const TABLE: &[Rule] = &[
    Rule {
        needles: &["claude", "opus"],
        info: ModelInfo::new(200_000, 15.0, 75.0),
    },
    Rule {
        needles: &["claude", "haiku"],
        info: ModelInfo::new(200_000, 1.0, 5.0),
    },
    Rule {
        needles: &["claude"],
        info: ModelInfo::new(200_000, 3.0, 15.0),
    },
    Rule {
        needles: &["sonnet"],
        info: ModelInfo::new(200_000, 3.0, 15.0),
    },
    Rule {
        needles: &["gpt-5"],
        info: ModelInfo::new(400_000, 1.25, 10.0),
    },
    Rule {
        needles: &["gpt5"],
        info: ModelInfo::new(400_000, 1.25, 10.0),
    },
    Rule {
        needles: &["o3"],
        info: ModelInfo::new(128_000, 2.5, 10.0),
    },
    Rule {
        needles: &["gpt-4.1"],
        info: ModelInfo::new(128_000, 2.5, 10.0),
    },
    Rule {
        needles: &["gpt-4o"],
        info: ModelInfo::new(128_000, 2.5, 10.0),
    },
    Rule {
        needles: &["grok-4"],
        info: ModelInfo::new(256_000, 2.0, 10.0),
    },
    Rule {
        needles: &["grok-3"],
        info: ModelInfo::new(256_000, 2.0, 10.0),
    },
    Rule {
        needles: &["composer"],
        info: ModelInfo::new(256_000, 2.0, 10.0),
    },
    Rule {
        needles: &["grok"],
        info: ModelInfo::new(131_072, 2.0, 10.0),
    },
];

/// Approximate metadata for a model id, or `None` when unknown. Ports `modelInfo`.
#[must_use]
pub fn model_info(model: &str) -> Option<ModelInfo> {
    let id = model.to_ascii_lowercase();
    TABLE
        .iter()
        .find(|rule| rule.needles.iter().all(|needle| id.contains(needle)))
        .map(|rule| rule.info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_context_window_for_known_families() {
        assert_eq!(
            model_info("claude-sonnet-4").unwrap().context_window,
            200_000
        );
        assert_eq!(model_info("grok-4-fast").unwrap().context_window, 256_000);
        assert!(model_info("gpt-5.5").unwrap().context_window >= 200_000);
        assert!(model_info("totally-unknown-model").is_none());
    }

    #[test]
    fn opus_beats_generic_claude() {
        // Most-specific-first: `claude...opus` must not fall through to the 3/15
        // generic-claude rule.
        let opus = model_info("claude-opus-4-8").unwrap();
        assert_eq!(opus.input_per_mtok, 15.0);
        assert_eq!(opus.output_per_mtok, 75.0);
        let haiku = model_info("claude-haiku-4-5").unwrap();
        assert_eq!(haiku.input_per_mtok, 1.0);
    }

    #[test]
    fn grok_specific_window_beats_generic_grok() {
        assert_eq!(model_info("grok-4-fast").unwrap().context_window, 256_000);
        assert_eq!(model_info("grok-2").unwrap().context_window, 131_072);
    }
}
