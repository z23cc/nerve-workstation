//! Inline SVG glyphs for the sidebar nav rows. Pure view fragments, split out of
//! `mod.rs` to keep the sidebar component under the file-size gate.

use leptos::prelude::*;

pub(super) fn thread_icon() -> impl IntoView {
    view! {
        <svg class="nav-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z" />
        </svg>
    }
}

pub(super) fn chat_icon() -> impl IntoView {
    view! {
        <svg class="nav-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M7 8h10" />
            <path d="M7 12h6" />
            <path d="M5 19a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3h14a3 3 0 0 1 3 3v9a3 3 0 0 1-3 3H9l-4 3z" />
        </svg>
    }
}

pub(super) fn automation_icon() -> impl IntoView {
    view! {
        <svg class="nav-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M13 2 4 14h7l-1 8 10-13h-7z" />
        </svg>
    }
}

pub(super) fn skill_icon() -> impl IntoView {
    view! {
        <svg class="nav-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M12 3v18" />
            <path d="M5 8h14" />
            <path d="M7 16h10" />
            <path d="M4 12h16" />
        </svg>
    }
}

pub(super) fn wechat_icon() -> impl IntoView {
    view! {
        <svg class="nav-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M9 4a6 6 0 0 0-6 6c0 2 1 3.7 2.7 4.8L5 18l3-1.4A8 8 0 0 0 9 16" />
            <path d="M14 8a6 6 0 0 1 6 6c0 1.7-.8 3.3-2.2 4.4L19 21l-2.6-1.2A7 7 0 0 1 14 20a6 6 0 0 1 0-12z" />
        </svg>
    }
}

pub(super) fn settings_icon() -> impl IntoView {
    view! {
        <svg class="nav-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z" />
            <path d="M3 12h2" />
            <path d="M19 12h2" />
            <path d="M12 3v2" />
            <path d="M12 19v2" />
        </svg>
    }
}
