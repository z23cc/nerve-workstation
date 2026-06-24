//! Shared transcript rendering helpers for the Leptos chat surface.
//!
//! The per-turn reactive renderers live in [`crate::transcript`]; this module
//! owns the stateless pieces both it and tests share: markdown → sanitized HTML,
//! tool cards, and the copy affordances.

use crate::clipboard::copy_text_with_note;
use crate::model::ToolCard;
use crate::trace_format::tool_trace;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

pub(crate) struct RenderedMarkdown {
    pub(crate) html: String,
    pub(crate) code_blocks: Vec<String>,
}

struct CodeCapture {
    language: Option<String>,
    source: String,
}

pub(crate) fn turn_copy_button(
    label: &'static str,
    text: String,
    note: RwSignal<String>,
) -> impl IntoView {
    view! {
        <button
            class="turn-copy"
            type="button"
            aria-label=label
            disabled=text.is_empty()
            on:click=move |_| {
                copy_text_with_note(text.clone(), note, "Copied message.");
            }
        >"Copy"</button>
    }
}

pub(crate) fn copy_status(note: RwSignal<String>) -> impl IntoView {
    view! {
        {move || (!note.get().is_empty()).then(|| view! {
            <span class="turn-copy-note" role="status">{note.get()}</span>
        })}
    }
}

pub(crate) fn copy_code_block(
    ev: leptos::ev::MouseEvent,
    code_blocks: StoredValue<Vec<String>>,
    note: RwSignal<String>,
) {
    let Some(button) = ev
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.closest(".md-code-copy").ok().flatten())
    else {
        return;
    };
    let Some(index) = button
        .get_attribute("data-code-index")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return;
    };
    ev.prevent_default();
    if let Some(code) = code_blocks.get_value().get(index).cloned() {
        copy_text_with_note(code, note, format!("Copied code block {}.", index + 1));
    }
}

pub(crate) fn render_tool(card: ToolCard) -> AnyView {
    let status = match card.ok {
        None => "run",
        Some(true) => "ok",
        Some(false) => "err",
    };
    let status_label = match card.ok {
        None => "running",
        Some(true) => "ok",
        Some(false) => "error",
    };
    let tool = card.tool;
    let input = card.input;
    let output = card.output;
    let has_trace = !input.is_empty() || !output.is_empty();
    let trace = if has_trace {
        tool_trace(&tool, status_label, &input, &output)
    } else {
        String::new()
    };
    let tool_label = format!("Tool call: {tool}, {status_label}");
    let trace_label = format!("Trace details for {tool}");
    let input_label = format!("Input trace for {tool}");
    let output_label = format!("Output trace for {tool}");
    let copy_note = RwSignal::new(String::new());
    view! {
        <div class=format!("tool {status}") role="group" aria-label=tool_label>
            <div class="tool-head">
                <span class="tool-name">{tool.clone()}</span>
                <div class="tool-head-actions">
                    {has_trace.then(|| view! {
                        <button
                            class="tool-copy"
                            type="button"
                            aria-label=format!("Copy trace for {tool}")
                            on:click={
                                let trace = trace.clone();
                                move |_| {
                                    copy_text_with_note(trace.clone(), copy_note, "Copied trace.");
                                }
                            }
                        >"Copy trace"</button>
                    })}
                    {move || (!copy_note.get().is_empty()).then(|| view! {
                        <span class="tool-copy-note" role="status">{copy_note.get()}</span>
                    })}
                    <span class=format!("tool-dot {status}") title=status_label aria-label=status_label></span>
                </div>
            </div>
            {has_trace.then(|| view! {
                <details class="tool-details" aria-label=trace_label>
                    <summary class="tool-summary" aria-label=format!("Toggle trace details for {tool}")>"trace"</summary>
                    {(!input.is_empty()).then(|| view! {
                        <div class="tool-detail-label">"input"</div>
                        <pre class="tool-out" aria-label=input_label>{input.clone()}</pre>
                    })}
                    {(!output.is_empty()).then(|| view! {
                        <div class="tool-detail-label">"output"</div>
                        <pre class="tool-out" aria-label=output_label>{output.clone()}</pre>
                    })}
                </details>
            })}
        </div>
    }
    .into_any()
}

pub(crate) fn markdown_to_html(src: &str) -> RenderedMarkdown {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut out = String::new();
    let mut events = Vec::new();
    let mut current_code: Option<CodeCapture> = None;
    let mut code_blocks = Vec::new();

    for event in Parser::new_ext(src, options) {
        if let Some(code) = current_code.as_mut() {
            match event {
                Event::End(TagEnd::CodeBlock) => {
                    let code = current_code.take().expect("code block is active");
                    push_code_block(&mut out, &mut code_blocks, code);
                }
                Event::Text(text)
                | Event::Code(text)
                | Event::Html(text)
                | Event::InlineHtml(text) => {
                    code.source.push_str(&text);
                }
                Event::SoftBreak | Event::HardBreak => code.source.push('\n'),
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_markdown_events(&mut out, &mut events);
                current_code = Some(CodeCapture {
                    language: code_language(&kind),
                    source: String::new(),
                });
            }
            Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Text(raw)),
            // Neutralize dangerous URL schemes in links/images — pulldown_cmark does
            // not filter them, so a model could emit `[x](javascript:…)` that becomes
            // a clickable XSS vector in the rendered DOM.
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => events.push(Event::Start(Tag::Link {
                link_type,
                dest_url: sanitize_url(dest_url),
                title,
                id,
            })),
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => events.push(Event::Start(Tag::Image {
                link_type,
                dest_url: sanitize_url(dest_url),
                title,
                id,
            })),
            other => events.push(other),
        }
    }

    if let Some(code) = current_code.take() {
        push_code_block(&mut out, &mut code_blocks, code);
    }
    flush_markdown_events(&mut out, &mut events);
    RenderedMarkdown {
        html: out,
        code_blocks,
    }
}

fn flush_markdown_events<'a>(out: &mut String, events: &mut Vec<pulldown_cmark::Event<'a>>) {
    if events.is_empty() {
        return;
    }
    pulldown_cmark::html::push_html(out, events.drain(..));
}

fn push_code_block(out: &mut String, code_blocks: &mut Vec<String>, code: CodeCapture) {
    let index = code_blocks.len();
    code_blocks.push(code.source.clone());
    let label = code.language.as_deref().unwrap_or("code");
    let class = code
        .language
        .as_ref()
        .map(|lang| format!(" class=\"language-{lang}\""))
        .unwrap_or_default();
    out.push_str("<div class=\"md-code-block\">");
    out.push_str("<div class=\"md-code-head\">");
    out.push_str("<span>");
    escape_html(out, label);
    out.push_str("</span>");
    out.push_str(&format!(
        "<button type=\"button\" class=\"md-code-copy\" data-code-index=\"{index}\" aria-label=\"Copy code block {}\">Copy code</button>",
        index + 1
    ));
    out.push_str("</div><pre><code");
    out.push_str(&class);
    out.push('>');
    escape_html(out, &code.source);
    out.push_str("</code></pre></div>");
}

fn code_language(kind: &pulldown_cmark::CodeBlockKind<'_>) -> Option<String> {
    let pulldown_cmark::CodeBlockKind::Fenced(info) = kind else {
        return None;
    };
    let lang = info.split_whitespace().next()?.trim();
    if lang.is_empty() {
        return None;
    }
    let safe = lang
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '+' | '.' | '#'))
        .collect::<String>();
    (!safe.is_empty()).then_some(safe)
}

fn escape_html(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str(r"&amp;"),
            '<' => out.push_str(r"&lt;"),
            '>' => out.push_str(r"&gt;"),
            '"' => out.push_str(r"&quot;"),
            '\'' => out.push_str(r"&#39;"),
            _ => out.push(ch),
        }
    }
}

/// Replace a dangerous URL scheme with a harmless placeholder. pulldown_cmark
/// passes link/image URLs through verbatim, so this is the GUI's guard against XSS
/// in model-authored markdown (the rendered HTML goes into `inner_html`).
fn sanitize_url<'a>(url: pulldown_cmark::CowStr<'a>) -> pulldown_cmark::CowStr<'a> {
    if is_dangerous_url(&url) {
        pulldown_cmark::CowStr::Borrowed("#")
    } else {
        url
    }
}

/// Whether `url` uses an executable/inline scheme, ignoring leading whitespace,
/// control chars, and case (so `java\tscript:`, ` JavaScript:` etc. are caught).
fn is_dangerous_url(url: &str) -> bool {
    let normalized: String = url
        .chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized.starts_with("javascript:")
        || normalized.starts_with("data:")
        || normalized.starts_with("vbscript:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_html_in_markdown_is_escaped() {
        let r = markdown_to_html("<script>alert(1)</script>\n\nhi");
        assert!(
            !r.html.contains("<script"),
            "raw <script> must be escaped, got: {}",
            r.html
        );
    }

    #[test]
    fn dangerous_url_schemes_are_neutralized() {
        for md in [
            "[click](javascript:alert(1))",
            "![x](javascript:alert(1))",
            "[x](vbscript:msgbox(1))",
            "[x](JAVASCRIPT:alert(1))",
            "[x](java\tscript:alert(1))",
            "[x](data:text/html,<script>alert(1)</script>)",
        ] {
            let r = markdown_to_html(md);
            let low = r.html.to_ascii_lowercase();
            assert!(
                !low.contains("javascript:")
                    && !low.contains("vbscript:")
                    && !low.contains("data:"),
                "dangerous scheme leaked for {md:?}: {}",
                r.html
            );
        }
    }

    #[test]
    fn safe_urls_are_preserved() {
        let r = markdown_to_html("[ok](https://example.com/path)");
        assert!(
            r.html.contains("https://example.com/path"),
            "safe URLs must be kept, got: {}",
            r.html
        );
        // Relative links are fine too.
        let r = markdown_to_html("[rel](./docs/readme.md)");
        assert!(r.html.contains("./docs/readme.md"), "got: {}", r.html);
    }

    #[test]
    fn is_dangerous_url_detection() {
        assert!(is_dangerous_url("javascript:alert(1)"));
        assert!(is_dangerous_url("  JavaScript:alert(1)"));
        assert!(is_dangerous_url("java\tscript:x"));
        assert!(is_dangerous_url("data:text/html,x"));
        assert!(!is_dangerous_url("https://example.com"));
        assert!(!is_dangerous_url("/abs/path"));
        assert!(!is_dangerous_url("mailto:x@y.z"));
    }
}
