//! Context-window overflow recovery.
//!
//! A request can exceed the model's context window despite [`compaction`] —
//! e.g. one enormous tool result, or an estimate that ran under the true token
//! count. Providers signal this differently (Anthropic: HTTP 413 / "prompt is
//! too long"; OpenAI/xAI: `context_length_exceeded` / "maximum context
//! length"). [`is_context_overflow`] classifies those signals from the surfaced
//! [`AgentError`] text as a single recoverable category, and
//! [`truncate_largest_tool_result`] frees room by stubbing the single biggest
//! tool-result body so the turn can be retried once.
//!
//! [`compaction`]: super::compaction

use crate::error::AgentError;
use crate::message::{Message, Role};

use super::compaction::ELIDED_TOOL_OUTPUT;

/// Substrings that, case-insensitively, mark a context-window overflow across
/// the supported providers. Kept small and provider-agnostic on purpose.
const OVERFLOW_MARKERS: &[&str] = &[
    "context_length_exceeded",
    "context length exceeded",
    "maximum context length",
    "prompt is too long",
    "too many tokens",
    "exceeds the maximum",
    "reduce the length",
    "input is too long",
];

/// Whether `error` looks like a context-window overflow that truncating history
/// could recover from. Recognizes HTTP 413 and the provider overflow phrases in
/// [`OVERFLOW_MARKERS`]. Conservative: anything unrecognized is *not* treated as
/// recoverable, so an unrelated 4xx still fails fast.
///
/// Only `Http`/`Provider` text is classified: an overflow never legitimately
/// surfaces as `Parse` (the HTTP layer checks status before parsing the body, so
/// a 413 is an `Http` error and stream/error events are `Provider`), so a
/// malformed-SSE body that merely quotes a marker can't trigger a lossy
/// truncate-and-retry. The 413 match is anchored to the `HTTP {status}: {body}`
/// shape the error formatter always produces, so a 4xx body that echoes "413"
/// in its text is not misread as overflow.
#[must_use]
pub(super) fn is_context_overflow(error: &AgentError) -> bool {
    let text = match error {
        AgentError::Http(msg) | AgentError::Provider(msg) => msg.as_str(),
        _ => return false,
    };
    let lower = text.to_ascii_lowercase();
    if lower.contains("http 413:") {
        return true;
    }
    OVERFLOW_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Stub the single largest elidable tool-result body in `history`, returning
/// `true` when something was truncated (so a retry is worthwhile) or `false`
/// when no further room can be freed this way.
///
/// Unlike the routine [`compaction`] pass — which protects a recent window to
/// keep working context — overflow is a hard failure, so this may truncate *any*
/// tool result, including a recent one, to get the turn through.
///
/// [`compaction`]: super::compaction
pub(super) fn truncate_largest_tool_result(history: &mut [Message]) -> bool {
    let largest = history
        .iter()
        .enumerate()
        .filter(|(_, msg)| is_truncatable(msg))
        .max_by_key(|(_, msg)| msg.content.len())
        .map(|(idx, _)| idx);
    match largest {
        Some(idx) => {
            history[idx].content = ELIDED_TOOL_OUTPUT.to_string();
            true
        }
        None => false,
    }
}

/// A tool result whose body is still a real payload (not already a stub) and so
/// can be truncated to free context.
fn is_truncatable(msg: &Message) -> bool {
    msg.role == Role::Tool && msg.content != ELIDED_TOOL_OUTPUT && !msg.content.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(msg: &str) -> AgentError {
        AgentError::Http(msg.to_string())
    }

    #[test]
    fn detects_openai_context_length_exceeded() {
        assert!(is_context_overflow(&AgentError::Provider(
            "error code: context_length_exceeded".into()
        )));
        assert!(is_context_overflow(&http(
            "HTTP 400: This model's maximum context length is 200000 tokens"
        )));
    }

    #[test]
    fn detects_anthropic_overflow_phrases_and_413() {
        assert!(is_context_overflow(&AgentError::Provider(
            "anthropic stream error: prompt is too long: 250000 tokens > 200000".into()
        )));
        assert!(is_context_overflow(&http("HTTP 413: Payload Too Large")));
    }

    #[test]
    fn unrelated_errors_are_not_overflow() {
        assert!(!is_context_overflow(&http("HTTP 401: unauthorized")));
        assert!(!is_context_overflow(&AgentError::Cancelled));
        assert!(!is_context_overflow(&AgentError::Tool("bad tool".into())));
    }

    #[test]
    fn parse_errors_are_not_overflow() {
        // A malformed-SSE / invalid-JSON decode failure that happens to quote an
        // overflow marker must NOT trigger a lossy truncate-and-retry: an overflow
        // never legitimately surfaces as a Parse error.
        assert!(!is_context_overflow(&AgentError::Parse(
            "invalid SSE event JSON: ... reduce the length ...".into()
        )));
    }

    #[test]
    fn non_413_body_echoing_413_is_not_overflow() {
        // The 413 match is anchored to the `HTTP 413:` status shape, so a 4xx
        // whose *body* echoes "413" / "status 413" is not misread as overflow.
        assert!(!is_context_overflow(&http(
            "HTTP 400: bad request (error code 413 mentioned in body)"
        )));
        assert!(!is_context_overflow(&http(
            "HTTP 400: upstream returned status 413 earlier"
        )));
    }

    #[test]
    fn truncates_the_single_largest_tool_body() {
        let mut history = vec![
            Message::user("ask"),
            Message::tool("a", "search", "small"),
            Message::tool("b", "search", "x".repeat(5_000)),
            Message::tool("c", "search", "medium-ish body"),
        ];
        assert!(truncate_largest_tool_result(&mut history));
        // The 5k body (index 2) is the one that got stubbed.
        assert_eq!(history[2].content, ELIDED_TOOL_OUTPUT);
        assert_eq!(history[1].content, "small");
        assert_eq!(history[3].content, "medium-ish body");
    }

    #[test]
    fn returns_false_when_nothing_left_to_truncate() {
        let mut history = vec![
            Message::user("ask"),
            Message::assistant("reply"),
            Message::tool("a", "search", ELIDED_TOOL_OUTPUT),
        ];
        assert!(!truncate_largest_tool_result(&mut history));
    }
}
