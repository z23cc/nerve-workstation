//! History compaction: bound the conversation by *token* count so it stays
//! within the provider context window without disturbing the warm cache prefix.
//!
//! ## Cache-preserving elision
//!
//! The Anthropic adapter stamps `cache_control` breakpoints on the last system
//! block, the last tool spec, and the last block of the *final* message
//! ([`super::super::provider::anthropic::wire`]). Prompt caching reads from the
//! cache up to the longest byte-identical prefix of the previous request, so the
//! cached span grows turn over turn **as long as the front of the message list
//! is byte-stable**. Mutating the *oldest* tool body (the previous strategy)
//! changes that prefix and busts every cached conversational token.
//!
//! So compaction is a **deterministic stub pass over the middle**: a leading
//! [`HISTORY_KEEP_PREFIX`] of messages is held byte-stable (the cache prefix)
//! and the trailing [`HISTORY_KEEP_RECENT`] messages are kept for working
//! context; only tool-result bodies *between* those two regions are stubbed,
//! newest-first, until the budget is met. No LLM call is involved.

use crate::message::{Message, Role};

use super::capabilities::ModelCapabilities;

/// Number of oldest messages held byte-stable so the warm cache prefix survives
/// compaction. These are never stubbed even when over budget.
pub(super) const HISTORY_KEEP_PREFIX: usize = 2;
/// Number of most-recent messages always preserved verbatim by compaction.
pub(super) const HISTORY_KEEP_RECENT: usize = 8;
/// Placeholder substituted for an elided tool output during compaction.
pub(super) const ELIDED_TOOL_OUTPUT: &str = "[tool output elided to fit context]";
/// Fraction of the context window we allow history to occupy before compacting,
/// leaving headroom for the system prompt, tool specs, and the next response.
const COMPACT_FRACTION: f64 = 0.70;

/// The token budget at which compaction should trigger for `caps`: ~70% of the
/// context window, then minus a reserve for the model's response so a near-full
/// history still leaves room to answer.
pub(super) fn compact_threshold_tokens(caps: &ModelCapabilities) -> usize {
    let window = caps.context_window;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let soft = (window as f64 * COMPACT_FRACTION) as usize;
    let reserve = caps.max_output_tokens as usize;
    // Keep a floor so a tiny/unknown window can't drive the threshold to 0.
    soft.saturating_sub(reserve).max(window / 4)
}

/// Bound the history to `threshold_tokens` with a deterministic stub pass.
///
/// Stubs elidable tool-result bodies in the *middle* window
/// `[HISTORY_KEEP_PREFIX .. len - HISTORY_KEEP_RECENT)`, newest-first, until the
/// budget is met or nothing elidable remains. The leading prefix (cache anchor)
/// and the trailing recent window are never touched, so the warm cache prefix
/// stays byte-identical across turns.
pub(super) fn compact_history(history: &mut [Message], threshold_tokens: usize) {
    let Some((middle_start, middle_end)) = middle_window(history.len()) else {
        return;
    };
    while history_tokens(history) > threshold_tokens {
        // Newest-first within the middle: freeing the most-recently-stale bodies
        // pushes the cache-invalidation boundary as late as possible.
        let target = history[middle_start..middle_end]
            .iter()
            .rposition(is_elidable_tool)
            .map(|offset| middle_start + offset);
        let Some(idx) = target else {
            break;
        };
        history[idx].content = ELIDED_TOOL_OUTPUT.to_string();
    }
}

/// The half-open `[start, end)` middle window that compaction may stub, or
/// `None` when the protected prefix and recent windows leave no middle.
fn middle_window(len: usize) -> Option<(usize, usize)> {
    let recent_end = len.checked_sub(HISTORY_KEEP_RECENT)?;
    if recent_end <= HISTORY_KEEP_PREFIX {
        return None;
    }
    Some((HISTORY_KEEP_PREFIX, recent_end))
}

/// A tool message whose body can still be replaced by the compaction placeholder.
pub(super) fn is_elidable_tool(msg: &Message) -> bool {
    msg.role == Role::Tool && msg.content != ELIDED_TOOL_OUTPUT
}

/// Approximate token footprint of the conversation, summing the token count of
/// each message's text and tool-call arguments. Used by the compaction guard.
pub(super) fn history_tokens(history: &[Message]) -> usize {
    history.iter().map(message_tokens).sum()
}

/// Token footprint of a single message: its content plus any tool-call argument
/// JSON. A small per-message overhead approximates role/structural framing.
fn message_tokens(msg: &Message) -> usize {
    const PER_MESSAGE_OVERHEAD: usize = 4;
    let mut tokens = nerve_core::token::count_tokens(&msg.content);
    for call in &msg.tool_calls {
        tokens += nerve_core::token::count_tokens(&call.name);
        tokens += nerve_core::token::count_tokens(&call.arguments.to_string());
    }
    tokens + PER_MESSAGE_OVERHEAD
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a history of `n` user/tool pairs with sizable tool bodies, plus a
    /// trailing block of recent messages that must stay untouched.
    fn history_with_big_tool_results(pairs: usize) -> Vec<Message> {
        let mut history = Vec::new();
        for i in 0..pairs {
            history.push(Message::user(format!("ask {i}")));
            history.push(Message::tool(
                format!("call_{i}"),
                "search",
                "x".repeat(2_000),
            ));
        }
        history
    }

    #[test]
    fn under_threshold_history_is_left_alone() {
        let mut history = history_with_big_tool_results(2);
        let before: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        // A generous budget: nothing should be elided.
        compact_history(&mut history, 1_000_000);
        let after: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        assert_eq!(after, before);
    }

    #[test]
    fn compaction_stubs_only_the_middle_preserving_cache_prefix() {
        let mut history = history_with_big_tool_results(12);
        let len = history.len();
        let threshold = 1_000; // far below the real footprint, forces elision
        compact_history(&mut history, threshold);

        let elided: Vec<usize> = history
            .iter()
            .enumerate()
            .filter(|(_, m)| m.content == ELIDED_TOOL_OUTPUT)
            .map(|(i, _)| i)
            .collect();
        assert!(!elided.is_empty(), "expected elision under a tight budget");
        // The leading prefix (cache anchor) is never stubbed: it stays byte-stable
        // across turns so the warm prefix keeps hitting.
        assert!(
            elided.iter().all(|&i| i >= HISTORY_KEEP_PREFIX),
            "prefix region must stay byte-stable, got {elided:?}"
        );
        // The trailing recent window is never stubbed.
        let recent_start = len - HISTORY_KEEP_RECENT;
        assert!(elided.iter().all(|&i| i < recent_start));
    }

    #[test]
    fn elision_is_newest_first_in_the_middle() {
        // Only enough budget pressure to force exactly one stub: it must land on
        // the newest elidable middle body, maximizing the surviving cache prefix.
        let mut history = history_with_big_tool_results(12);
        let len = history.len();
        let (start, end) = middle_window(len).expect("a middle window exists");
        // Pick a threshold just under the full footprint so one elision suffices.
        let full = history_tokens(&history);
        let one_body = nerve_core::token::count_tokens(&"x".repeat(2_000));
        compact_history(&mut history, full - one_body / 2);

        // Exactly one body stubbed, and it is the last elidable index in the middle.
        let elided: Vec<usize> = (start..end)
            .filter(|&i| history[i].content == ELIDED_TOOL_OUTPUT)
            .collect();
        assert_eq!(elided.len(), 1, "expected a single stub, got {elided:?}");
        let newest_middle_tool = (start..end)
            .rev()
            .find(|&i| history[i].role == Role::Tool)
            .expect("a tool message in the middle");
        assert_eq!(elided[0], newest_middle_tool);
    }

    #[test]
    fn compaction_is_idempotent_on_already_stubbed_history() {
        // Running compaction twice yields the same set of stubs: the placeholder
        // is not itself elidable, so the second pass is a no-op (deterministic).
        let mut history = history_with_big_tool_results(12);
        compact_history(&mut history, 1_000);
        let after_first: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        compact_history(&mut history, 1_000);
        let after_second: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn keeps_recent_messages_even_when_over_budget() {
        // Only recent messages present: nothing is elidable.
        let mut history = history_with_big_tool_results(3);
        assert!(history.len() <= HISTORY_KEEP_RECENT + 1);
        let before = history.clone();
        compact_history(&mut history, 1);
        // The recent window is protected; with so few messages, nothing changes.
        let recent_unchanged = before
            .iter()
            .rev()
            .take(HISTORY_KEEP_RECENT)
            .zip(history.iter().rev().take(HISTORY_KEEP_RECENT))
            .all(|(a, b)| a.content == b.content);
        assert!(recent_unchanged);
    }

    #[test]
    fn threshold_scales_with_context_window() {
        let small = ModelCapabilities {
            context_window: 128_000,
            max_output_tokens: 8_192,
        };
        let large = ModelCapabilities {
            context_window: 400_000,
            max_output_tokens: 128_000,
        };
        assert!(compact_threshold_tokens(&large) > compact_threshold_tokens(&small));
        // Reserve never drives the threshold to zero (the window/4 floor holds).
        let tight = ModelCapabilities {
            context_window: 8_000,
            max_output_tokens: 8_000,
        };
        assert!(compact_threshold_tokens(&tight) >= 8_000 / 4);
    }
}
