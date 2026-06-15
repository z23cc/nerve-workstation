//! Pure-Rust token counting helpers.

use std::sync::OnceLock;
use tiktoken_rs::{CoreBPE, cl100k_base, o200k_base};

/// Count tokens with a reusable tiktoken BPE.
///
/// `o200k_base` is preferred for modern OpenAI context estimates. If the BPE
/// table cannot be constructed for any reason, fall back to a documented,
/// deterministic pure-Rust estimator. Token counts are estimates for budget
/// planning, not a protocol boundary.
#[must_use]
pub fn count_tokens(text: &str) -> usize {
    tokenizer().map_or_else(
        || estimate_tokens(text),
        |bpe| bpe.encode_ordinary(text).len(),
    )
}

fn tokenizer() -> Option<&'static CoreBPE> {
    static TOKENIZER: OnceLock<Option<CoreBPE>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| o200k_base().or_else(|_| cl100k_base()).ok())
        .as_ref()
}

fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::count_tokens;

    #[test]
    fn count_tokens_is_stable_for_ascii_text() {
        assert_eq!(count_tokens("hello world"), 2);
        assert_eq!(count_tokens(""), 0);
    }
}
