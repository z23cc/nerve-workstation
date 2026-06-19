//! The multiline input editor: value + cursor with grapheme-correct editing and
//! submission history. Ports `packages/tui/src/ui/editor.ts` (`layout`) and the
//! input/key/history mechanics from `app.ts` (`#insert`, backspace, ctrl-u,
//! ctrl-w, left/right/home/end, `#pushHistory`/`#historyPrev`/`#historyNext`).
//!
//! The TS held cursor as a UTF-16 code-unit index into a JS string; here the
//! cursor is a **byte** index into the Rust `String` and all movement snaps to
//! grapheme-cluster boundaries via `unicode-segmentation`, so a multi-codepoint
//! emoji or a combining sequence moves/deletes as one unit. Display width comes
//! from `unicode-width` (CJK = 2 cols), matching the renderer.

use unicode_segmentation::UnicodeSegmentation;

use super::width::width;

/// One display layout of the input: rows (newline-split) and the cursor's
/// (row, col) where `col` is a display-column offset. Ports `EditorLayout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorLayout {
    pub rows: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

/// Map a `value` + byte-`cursor` to display rows and a (row, col) cursor. Ports
/// `editor.ts::layout`: one row per newline-separated logical line, the cursor
/// row is the count of newlines before the cursor, the column is the display
/// width of the text from the line start to the cursor.
#[must_use]
pub fn layout(value: &str, cursor: usize) -> EditorLayout {
    let rows: Vec<String> = value.split('\n').map(str::to_string).collect();
    let cursor = cursor.min(value.len());
    let before = &value[..cursor];
    let cursor_row = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |idx| idx + 1);
    let cursor_col = width(&before[line_start..]);
    EditorLayout {
        rows,
        cursor_row,
        cursor_col,
    }
}

/// The multiline input editor: the current value, a byte-cursor into it, and the
/// submission history with a navigation index.
#[derive(Debug, Clone, Default)]
pub struct Editor {
    value: String,
    cursor: usize,
    /// Prior submissions, oldest first. Navigated with up/down.
    history: Vec<String>,
    /// `-1` (here: `None`) means "editing live input, not browsing history".
    history_index: Option<usize>,
}

impl Editor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// The (row, col) layout for the current value/cursor.
    #[must_use]
    pub fn layout(&self) -> EditorLayout {
        layout(&self.value, self.cursor)
    }

    /// Replace the whole value and place the cursor at its end (used by history
    /// navigation and palette completion).
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
    }

    /// Clear the value, the cursor, and reset history browsing. Returns the text
    /// that was taken (trimmed by the caller as needed).
    pub fn clear(&mut self) -> String {
        self.history_index = None;
        self.cursor = 0;
        std::mem::take(&mut self.value)
    }

    /// Insert text at the cursor (typed char, pasted block, or a `\n` for
    /// alt/shift-enter). Resets history browsing. Ports `#insert`.
    pub fn insert(&mut self, text: &str) {
        self.value.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.history_index = None;
    }

    /// Delete the grapheme before the cursor (Backspace). Ports the `#onKey`
    /// backspace arm. Resets history browsing.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_grapheme_boundary(self.cursor);
        self.value.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.history_index = None;
    }

    /// Kill the whole line (Ctrl-U). The TS clears the entire input; we match it.
    pub fn kill_line(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    /// Kill the word before the cursor (Ctrl-W): skip trailing spaces, then the
    /// run of non-space graphemes. Resets history browsing. (The TS `keys.ts`
    /// decodes Ctrl-W but `app.ts` left it unhandled; this is the documented
    /// readline behavior the task asks for.)
    pub fn kill_word(&mut self) {
        let mut start = self.cursor;
        // Skip whitespace immediately before the cursor.
        while start > 0 {
            let prev = self.prev_grapheme_boundary(start);
            if self.value[prev..start].chars().all(char::is_whitespace) {
                start = prev;
            } else {
                break;
            }
        }
        // Then delete the run of non-whitespace graphemes.
        while start > 0 {
            let prev = self.prev_grapheme_boundary(start);
            if self.value[prev..start].chars().all(char::is_whitespace) {
                break;
            }
            start = prev;
        }
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.history_index = None;
    }

    /// Move the cursor one grapheme left.
    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_grapheme_boundary(self.cursor);
        }
    }

    /// Move the cursor one grapheme right.
    pub fn right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor = self.next_grapheme_boundary(self.cursor);
        }
    }

    /// Move the cursor to the start of the value (Home). The TS Home is
    /// whole-input start, not line start; we match it.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end of the value (End).
    pub fn end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Whether the cursor is on the first display row (drives the app's rule that
    /// Up only navigates history from the first line). Ports the `app.ts` check
    /// `layout(...).cursorRow === 0` implicit in history nav.
    #[must_use]
    pub fn cursor_on_first_row(&self) -> bool {
        !self.value[..self.cursor].contains('\n')
    }

    /// Record a submission in history (dedup against the immediate previous) and
    /// reset the browse index. Ports `#pushHistory`.
    pub fn push_history(&mut self, text: &str) {
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
        }
        self.history_index = None;
    }

    /// Navigate to the previous (older) history entry (Up). Ports `#historyPrev`:
    /// first Up jumps to the newest entry; further Ups walk back, clamped at the
    /// oldest. No-op on empty history.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => self.history.len() - 1,
            Some(idx) => idx.saturating_sub(1),
        };
        self.history_index = Some(index);
        self.set_value(self.history[index].clone());
    }

    /// Navigate to the next (newer) history entry (Down). Ports `#historyNext`:
    /// stepping past the newest entry returns to an empty live input. No-op when
    /// not currently browsing history.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        let next = index + 1;
        if next >= self.history.len() {
            self.history_index = None;
            self.set_value(String::new());
        } else {
            self.history_index = Some(next);
            self.set_value(self.history[next].clone());
        }
    }

    /// Reset history browsing without touching the value (used after edits that
    /// already reset it, kept for the palette/insert paths needing it explicitly).
    pub fn reset_history_browse(&mut self) {
        self.history_index = None;
    }

    fn prev_grapheme_boundary(&self, byte: usize) -> usize {
        self.value[..byte]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(idx, _)| idx)
    }

    fn next_grapheme_boundary(&self, byte: usize) -> usize {
        self.value[byte..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.value.len(), |(idx, _)| byte + idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_maps_cursor_across_newlines() {
        let l = layout("ab\ncde", 5);
        assert_eq!(l.rows, vec!["ab", "cde"]);
        assert_eq!(l.cursor_row, 1);
        assert_eq!(l.cursor_col, 2);
    }

    #[test]
    fn layout_counts_wide_chars_in_col() {
        // Two CJK chars before the cursor = 4 display columns.
        let l = layout("你好x", "你好".len());
        assert_eq!(l.cursor_col, 4);
    }

    #[test]
    fn insert_and_cursor_movement() {
        let mut e = Editor::new();
        e.insert("hello");
        assert_eq!((e.value(), e.cursor()), ("hello", 5));
        e.left();
        e.left();
        assert_eq!(e.cursor(), 3);
        e.insert("X");
        assert_eq!(e.value(), "helXlo");
        e.home();
        assert_eq!(e.cursor(), 0);
        e.end();
        assert_eq!(e.cursor(), 6);
    }

    #[test]
    fn backspace_deletes_one_grapheme() {
        let mut e = Editor::new();
        e.insert("ab");
        e.backspace();
        assert_eq!(e.value(), "a");
        e.backspace();
        assert!(e.is_empty());
        e.backspace(); // no-op at start
        assert!(e.is_empty());
    }

    #[test]
    fn backspace_handles_multibyte_grapheme() {
        let mut e = Editor::new();
        e.insert("a你");
        e.backspace();
        assert_eq!(e.value(), "a");
    }

    #[test]
    fn kill_line_clears_everything() {
        let mut e = Editor::new();
        e.insert("some text");
        e.left();
        e.kill_line();
        assert!(e.is_empty());
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn kill_word_deletes_prev_word_and_trailing_space() {
        let mut e = Editor::new();
        e.insert("foo bar baz");
        e.kill_word();
        assert_eq!(e.value(), "foo bar ");
        e.kill_word();
        assert_eq!(e.value(), "foo ");
    }

    #[test]
    fn cursor_on_first_row_tracks_newlines() {
        let mut e = Editor::new();
        e.insert("a\nb");
        assert!(!e.cursor_on_first_row());
        e.home();
        assert!(e.cursor_on_first_row());
    }

    #[test]
    fn history_up_down_walks_submissions() {
        let mut e = Editor::new();
        e.push_history("first");
        e.push_history("second");
        e.history_prev();
        assert_eq!(e.value(), "second");
        e.history_prev();
        assert_eq!(e.value(), "first");
        e.history_prev(); // clamp at oldest
        assert_eq!(e.value(), "first");
        e.history_next();
        assert_eq!(e.value(), "second");
        e.history_next(); // past newest → empty live input
        assert_eq!(e.value(), "");
        e.history_next(); // no-op when not browsing
        assert_eq!(e.value(), "");
    }

    #[test]
    fn history_dedups_consecutive_and_resets_on_insert() {
        let mut e = Editor::new();
        e.push_history("dup");
        e.push_history("dup");
        e.history_prev();
        assert_eq!(e.value(), "dup");
        // Editing resets the browse index, so the next Up starts from newest.
        e.insert("!");
        e.history_prev();
        assert_eq!(e.value(), "dup");
    }
}
