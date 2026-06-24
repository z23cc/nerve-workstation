//! [`Block::WechatBridge`] → styled lines.
//!
//! `image_url` is NOT an image — it is an iLink deep-link URL the user scans
//! with WeChat. Ratatui cannot render inline images, so we QR-ENCODE that URL
//! locally (the pure-Rust `qrcode` crate, unicode block renderer) into scannable
//! text lines, plus a small caption line carrying the raw URL + id. The panel
//! renders a compact header, status line, the QR (when present), and the last N
//! messages from the rolling log.

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::palette;
use super::width::{sanitize, wrap_styled};

/// Maximum number of message-log lines shown in the bridge panel.
const VISIBLE_MESSAGES: usize = 10;

/// Render the WeChat bridge panel. Layout:
///
/// ```text
/// 📱 wechat bridge · <status>
/// █▀▀▀▀▀█ ... (scannable QR encoded from the deep-link url)
/// scan QR: <url> (id <id>)      ← only when a QR has been received
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

    // QR (shown only when a QR has been received): a scannable block-rendered QR
    // encoded from the deep-link url, then a caption with the raw url + id.
    if let (Some(id), Some(url)) = (qr_id, qr_url) {
        lines.extend(qr_code_lines(url));
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

/// QR-encode `url` into block-character lines (`qrcode` `unicode::Dense1x2`),
/// styled plain so the dark/light blocks stay scannable. Generated locally — the
/// url is never sent anywhere. A url the encoder rejects (too long / invalid)
/// yields no lines, so the caller's caption line is the graceful fallback.
fn qr_code_lines(url: &str) -> Vec<Line<'static>> {
    let Ok(code) = QrCode::new(url.as_bytes()) else {
        return Vec::new();
    };
    let rendered = code
        .render::<Dense1x2>()
        .quiet_zone(true)
        .module_dimensions(1, 1)
        .build();
    // Default style: terminal default fg on default bg keeps the dark/light
    // blocks at the contrast a scanner needs (no palette recoloring).
    rendered
        .lines()
        .map(|row| Line::from(Span::styled(row.to_string(), Style::default())))
        .collect()
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
    fn render_bridge_shows_qr_code_caption_and_id() {
        let lines = render_wechat_bridge(
            "scan me",
            Some("qr-abc"),
            Some("https://liteapp.weixin.qq.com/q/7GiQu1?qrcode=abc&bot_type=3"),
            &[],
            60,
        );
        let text = plain(&lines);
        // Caption keeps the raw url + id.
        assert!(text.contains("liteapp.weixin.qq.com"), "{text}");
        assert!(text.contains("qr-abc"), "{text}");
        // And a scannable QR is rendered from the url (block characters present).
        assert!(text.contains('█'), "expected QR blocks, got: {text}");
    }

    #[test]
    fn qr_code_lines_renders_blocks_and_is_nonempty() {
        let lines = qr_code_lines("https://liteapp.weixin.qq.com/q/7GiQu1?qrcode=abc");
        assert!(!lines.is_empty());
        let text = plain(&lines);
        assert!(text.contains('█'), "{text}");
    }

    #[test]
    fn qr_code_lines_too_long_url_yields_no_lines() {
        // A url too long for any QR version must not panic — it returns no lines
        // so the caller's caption line is the graceful fallback.
        assert!(qr_code_lines(&"x".repeat(8000)).is_empty());
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
