use super::*;

pub(super) fn build_search_regex(
    request: &SearchRequest,
    case_sensitive: bool,
) -> Result<Option<Regex>, regex::Error> {
    if request.regex {
        RegexBuilder::new(&request.pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn build_literal_matcher(
    request: &SearchRequest,
    case_sensitive: bool,
) -> Option<AhoCorasick> {
    if request.regex || request.pattern.is_empty() {
        return None;
    }
    Some(
        AhoCorasickBuilder::new()
            .ascii_case_insensitive(!case_sensitive)
            .build([request.pattern.as_str()])
            .expect("single literal pattern"),
    )
}

pub(super) fn build_fuzzy_pattern(
    request: &SearchRequest,
    case_sensitive: bool,
) -> Option<Pattern> {
    (!request.regex).then(|| {
        Pattern::new(
            &request.pattern,
            nucleo_case_matching(case_sensitive),
            Normalization::Never,
            AtomKind::Fuzzy,
        )
    })
}

pub(super) fn first_regex_match(
    text: &str,
    regex: &Regex,
    whole_word: bool,
) -> Option<(usize, usize)> {
    regex.find_iter(text).find_map(|mat| {
        let span = (mat.start(), mat.end());
        (!whole_word || is_whole_word_match(text, span.0, span.1)).then_some(span)
    })
}

pub(super) fn first_literal_match(
    text: &str,
    pattern: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Option<(usize, usize)> {
    if pattern.is_empty() {
        return Some((0, 0));
    }

    let mut offset = 0usize;
    while offset <= text.len().saturating_sub(pattern.len()) {
        let found = if case_sensitive {
            text[offset..].find(pattern)
        } else {
            find_ascii_case_insensitive(&text.as_bytes()[offset..], pattern.as_bytes())
        };
        let start = offset + found?;
        let end = start + pattern.len();
        if !whole_word || is_whole_word_match(text, start, end) {
            return Some((start, end));
        }
        offset = next_char_boundary(text, start);
    }
    None
}

pub(super) fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

pub(super) fn next_char_boundary(text: &str, byte_idx: usize) -> usize {
    text[byte_idx..]
        .char_indices()
        .nth(1)
        .map_or(text.len(), |(offset, _)| byte_idx + offset)
}

pub(super) fn is_smart_case_sensitive(pattern: &str) -> bool {
    pattern.chars().any(char::is_uppercase)
}

pub(super) fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub(super) fn is_whole_word_match(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_none_or(|ch| !is_word_char(ch)) && after.is_none_or(|ch| !is_word_char(ch))
}
