//! Stick-to-bottom scroll anchoring for the chat transcript.
//!
//! The pin state is **derived from the user's scroll position**, never forced: a
//! streaming delta only ever *reads* `pinned`, so auto-scroll can never fight a
//! user who has scrolled up to read history. The follow scroll is a direct
//! `scrollTop` assignment, which is **always instant** (CSS `scroll-behavior`
//! governs only `scrollTo`/`scrollBy`/`scrollIntoView` and keyboard scrolling, not
//! a `scrollTop` write) — so it is motion-safe by construction and cannot lag
//! behind a fast stream and spuriously release the pin.

use leptos::html::Div;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

/// Re-arm the pin when the viewport bottom is within this many CSS px of the
/// content bottom. Generous enough to survive sub-pixel rounding and the
/// streaming cursor glyph, tight enough that a deliberate scroll-up releases.
const STICK_THRESHOLD_PX: f64 = 64.0;

/// A `Copy` controller (lives in `App` state) that pins the transcript to its
/// bottom while content streams and exposes a "Jump to latest" intent.
#[derive(Clone, Copy)]
pub(crate) struct ScrollAnchor {
    /// The scroll container (`.transcript`); `None` until it mounts.
    pub(crate) container: NodeRef<Div>,
    /// True while the view is stuck to the bottom (auto-scroll armed).
    pub(crate) pinned: RwSignal<bool>,
    /// Coalesces N streaming deltas into one scroll write per animation frame.
    frame_pending: StoredValue<bool>,
}

impl ScrollAnchor {
    pub(crate) fn new() -> Self {
        Self {
            container: NodeRef::new(),
            pinned: RwSignal::new(true),
            frame_pending: StoredValue::new(false),
        }
    }

    /// Recompute the pin from the live scroll position. Called from `on:scroll`.
    pub(crate) fn on_user_scroll(&self) {
        if let Some(el) = self.container.get_untracked() {
            self.pinned.set(is_near_bottom(
                f64::from(el.scroll_top()),
                f64::from(el.scroll_height()),
                f64::from(el.client_height()),
            ));
        }
    }

    /// Force the view to the bottom and re-arm the pin (Jump button / send /
    /// thread switch). Idempotent; a no-op before the node mounts.
    pub(crate) fn snap_to_bottom(&self) {
        self.pinned.set(true);
        self.schedule_scroll();
    }

    /// If pinned, scroll to the bottom after the new content paints — coalesced
    /// to one write per animation frame regardless of delta rate.
    pub(crate) fn follow_if_pinned(&self) {
        if self.pinned.get_untracked() {
            self.schedule_scroll();
        }
    }

    fn schedule_scroll(&self) {
        if self.frame_pending.get_value() {
            return; // a frame is already scheduled
        }
        self.frame_pending.set_value(true);
        let anchor = *self;
        let cb = Closure::once_into_js(move || {
            anchor.frame_pending.set_value(false);
            anchor.scroll_to_bottom_now();
        });
        if let Some(win) = web_sys::window() {
            let _ = win.request_animation_frame(cb.unchecked_ref());
        }
    }

    fn scroll_to_bottom_now(&self) {
        if let Some(el) = self.container.get_untracked() {
            // A `scrollTop` write is instant (not animated by CSS scroll-behavior),
            // so the pin can't self-release mid-stream.
            let height = el.scroll_height();
            el.set_scroll_top(height);
        }
    }
}

/// Pure, host-testable: is the viewport bottom within the stick threshold?
pub(crate) fn is_near_bottom(scroll_top: f64, scroll_height: f64, client_height: f64) -> bool {
    (scroll_height - client_height - scroll_top) <= STICK_THRESHOLD_PX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_bottom_is_pinned() {
        assert!(is_near_bottom(900.0, 1000.0, 100.0)); // exactly bottom
        assert!(is_near_bottom(860.0, 1000.0, 100.0)); // 40px up, within 64
    }

    #[test]
    fn scrolled_up_releases() {
        assert!(!is_near_bottom(700.0, 1000.0, 100.0)); // 200px up
    }

    #[test]
    fn short_content_is_always_pinned() {
        assert!(is_near_bottom(0.0, 80.0, 100.0)); // content shorter than viewport
    }
}
