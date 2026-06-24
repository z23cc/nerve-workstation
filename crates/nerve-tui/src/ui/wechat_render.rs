//! [`Block::WechatBridge`] → styled lines.
//!
//! Ratatui cannot render inline images, so the QR is shown as a text URL + id.
//! The panel renders a compact header, status line, optional QR info, and the
//! last N messages from the rolling log.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::palette;
use super::width::{sanitize, wrap_styled};

/// Maximum number of message-log lines shown in the bridge panel.
const VISIBLE_MESSAGES: usize = 10;

/// Render the WeChat bridge panel. Layout:
///
/// ```text
/// 📱 wechat bridge · <status>
/// QR: <url> (id <id>)           ← only when a QR has been received
/// ── messages ──────────────────
/// in: hello agent
/// out: on it
/// ```
#[must_use]
pub fn render_wechat_bridge(
    status: &str,
    qr_id: Option<&str>,
    qr_url: Option<&str>,
    messages: &[String],
    cols: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Header row
    lines.push(Line::from(vec![
        Span::styled(
            "📱 wechat bridge".to_string(),
            palette::cyan().add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ".to_string(), palette::dim()),
        Span::styled(sanitize(status), palette::dim()),
    ]));

    // QR info (shown only when a QR has been received)
    if let (Some(id), Some(url)) = (qr_id, qr_url) {
        let qr_line = format!("scan QR: {} (id {})", sanitize(url), sanitize(id));
        lines.extend(wrap_styled(&qr_line, cols, palette::yellow()));
    }

    // Message log — only shown when there are messages to show
    if !messages.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("── messages {}", "─".repeat(cols.saturating_sub(14))),
            palette::dim(),
        )));
        let start = messages.len().saturating_sub(VISIBLE_MESSAGES);
        for msg in &messages[start..] {
            let style = if msg.starts_with("out:") {
                palette::cyan()
            } else {
                palette::dim()
            };
            lines.extend(
                wrap_styled(&sanitize(msg), cols.saturating_sub(2).max(1), style)
                    .into_iter()
                    .map(|line| {
                        let mut spans = vec![Span::styled("┊ ".to_string(), palette::dim())];
                        spans.extend(line.spans);
                        Line::from(spans)
                    }),
            );
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn render_bridge_shows_header_and_status() {
        let lines = render_wechat_bridge("awaiting scan", None, None, &[], 60);
        let text = plain(&lines);
        assert!(text.contains("wechat bridge"), "{text}");
        assert!(text.contains("awaiting scan"), "{text}");
        // No QR section without a QR.
        assert!(!text.contains("scan QR:"), "{text}");
        // No message section without messages.
        assert!(!text.contains("messages"), "{text}");
    }

    #[test]
    fn render_bridge_shows_qr_url_and_id() {
        let lines = render_wechat_bridge(
            "scan me",
            Some("qr-abc"),
            Some("https://example.com/qr.png"),
            &[],
            60,
        );
        let text = plain(&lines);
        assert!(text.contains("qr.png"), "{text}");
        assert!(text.contains("qr-abc"), "{text}");
    }

    #[test]
    fn render_bridge_shows_message_log() {
        let msgs: Vec<String> = vec!["in: hello".into(), "out: hi".into()];
        let lines = render_wechat_bridge("running", None, None, &msgs, 60);
        let text = plain(&lines);
        assert!(text.contains("messages"), "{text}");
        assert!(text.contains("in: hello"), "{text}");
        assert!(text.contains("out: hi"), "{text}");
    }

    #[test]
    fn render_bridge_caps_visible_messages_at_10() {
        let msgs: Vec<String> = (0..20).map(|i| format!("in: msg {i}")).collect();
        let lines = render_wechat_bridge("running", None, None, &msgs, 60);
        let text = plain(&lines);
        // Only the last 10 messages are shown.
        assert!(!text.contains("msg 0"), "{text}");
        assert!(text.contains("msg 10"), "{text}");
        assert!(text.contains("msg 19"), "{text}");
    }
}
