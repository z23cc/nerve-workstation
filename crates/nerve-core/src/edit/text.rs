//! Shared text helpers for the edit engine: newline/BOM normalization, a
//! content hash for stale-edit detection, and short error previews.

/// Original newline style of a file, so edits can be written back unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Newline {
    Lf,
    Crlf,
}

/// Detect the dominant newline style. CRLF wins if any `\r\n` is present.
pub(crate) fn detect_newline(text: &str) -> Newline {
    if text.contains("\r\n") {
        Newline::Crlf
    } else {
        Newline::Lf
    }
}

/// Strip a leading UTF-8 BOM and collapse CRLF/CR to LF. All edit modes operate
/// on LF-normalized text so line math is uniform.
pub(crate) fn normalize(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore the original newline style on LF-normalized output.
pub(crate) fn restore_newline(text: &str, newline: Newline) -> String {
    match newline {
        Newline::Lf => text.to_string(),
        Newline::Crlf => text.replace('\n', "\r\n"),
    }
}

/// 16-hex content tag (64-bit FNV-1a) over LF-normalized text with trailing
/// whitespace trimmed per line (so cosmetic trailing spaces never invalidate a
/// tag — matching the upstream hashline format). The hashline mode binds each
/// patch to this tag so a stale edit is rejected before it can corrupt a file.
/// Not cryptographic: the guarantee is probabilistic — two distinct files
/// collide on the same tag only by accident, with odds ~1/1.8e19 (full 64-bit
/// FNV-1a width).
pub(crate) fn content_hash(normalized: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for line in normalized.split('\n') {
        for byte in line.trim_end_matches([' ', '\t']).bytes() {
            mix(byte);
        }
        mix(b'\n');
    }
    format!("{hash:016X}")
}

/// A one-line, length-capped preview of a snippet for error messages.
pub(crate) fn preview(text: &str) -> String {
    const MAX: usize = 60;
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > MAX {
        let capped: String = line.chars().take(MAX - 1).collect();
        format!("{capped}\u{2026}")
    } else {
        line.to_string()
    }
}
