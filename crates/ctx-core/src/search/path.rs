use super::*;

pub(super) const PATH_SUBSTRING_BASE: i64 = 1_000_000;
pub(super) const PATH_REGEX_BASE: i64 = 900_000;

pub(super) fn collect_path_matches(
    snapshot: &CatalogSnapshot,
    filter: &EntryFilter,
    path_query: PathQuery<'_>,
    case_sensitive: bool,
    max_results: usize,
    cancel: &CancelToken,
) -> Result<(Vec<PathSearchMatch>, usize), CtxError> {
    let path_results = path_search_results(snapshot, filter, path_query, case_sensitive, cancel);
    let mut path_hits = Vec::new();
    for result in path_results {
        if let Some(hit) = result? {
            path_hits.push(hit);
        }
    }
    let total = path_hits.len();
    path_hits.sort_by(path_match_cmp);
    if path_hits.len() > max_results {
        path_hits.truncate(max_results);
    }
    Ok((path_hits, total))
}

pub(super) fn path_search_results(
    snapshot: &CatalogSnapshot,
    filter: &EntryFilter,
    path_query: PathQuery<'_>,
    case_sensitive: bool,
    cancel: &CancelToken,
) -> Vec<Result<Option<PathSearchMatch>, CtxError>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        snapshot
            .entries
            .par_iter()
            .map_init(
                || PathMatcherState::new(case_sensitive),
                |state, entry| path_search_hit(snapshot, filter, path_query, state, entry, cancel),
            )
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut state = PathMatcherState::new(case_sensitive);
        snapshot
            .entries
            .iter()
            .map(|entry| path_search_hit(snapshot, filter, path_query, &mut state, entry, cancel))
            .collect()
    }
}

pub(super) fn path_search_hit(
    snapshot: &CatalogSnapshot,
    filter: &EntryFilter,
    path_query: PathQuery<'_>,
    state: &mut PathMatcherState,
    entry: &CatalogEntry,
    cancel: &CancelToken,
) -> Result<Option<PathSearchMatch>, CtxError> {
    cancel.check_cancelled()?;
    if !filter.accepts(&entry.rel_path) {
        return Ok(None);
    }
    Ok(
        path_score(&entry.rel_path, path_query, state).map(|score| PathSearchMatch {
            root_id: entry.root_id.clone(),
            path: entry.rel_path.clone(),
            display_path: display_path(snapshot, &entry.root_id, &entry.rel_path),
            score,
        }),
    )
}

pub(super) fn path_match_cmp(
    left: &PathSearchMatch,
    right: &PathSearchMatch,
) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.path.cmp(&right.path))
}

pub(super) fn content_match_cmp(
    left: &ContentSearchMatch,
    right: &ContentSearchMatch,
) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.line.cmp(&right.line))
}

pub(super) fn display_path(snapshot: &CatalogSnapshot, root_id: &str, rel_path: &str) -> String {
    let Some(root) = snapshot.roots.iter().find(|root| root.id == root_id) else {
        return rel_path.to_string();
    };
    let root_name = root.path.file_name().unwrap_or_default().to_string_lossy();
    if rel_path.is_empty() {
        root_name.into_owned()
    } else {
        format!("{root_name}/{rel_path}")
    }
}

#[derive(Clone, Copy)]
pub(super) struct PathQuery<'a> {
    pub(super) pattern: &'a str,
    pub(super) regex: Option<&'a Regex>,
    pub(super) case_sensitive: bool,
    pub(super) whole_word: bool,
    pub(super) fuzzy_pattern: Option<&'a Pattern>,
}

pub(super) struct PathMatcherState {
    matcher: Matcher,
    utf32_buf: Vec<char>,
    indices: Vec<u32>,
}

impl PathMatcherState {
    pub(super) fn new(case_sensitive: bool) -> Self {
        let mut config = Config::DEFAULT.match_paths();
        config.ignore_case = !case_sensitive;
        config.normalize = false;
        config.prefer_prefix = true;
        Self {
            matcher: Matcher::new(config),
            utf32_buf: Vec::new(),
            indices: Vec::new(),
        }
    }
}

pub(super) fn path_score(
    path: &str,
    query: PathQuery<'_>,
    state: &mut PathMatcherState,
) -> Option<i64> {
    if query.pattern.is_empty() {
        return Some(0);
    }
    if let Some(regex) = query.regex {
        return first_regex_match(path, regex, query.whole_word)
            .map(|(start, _end)| PATH_REGEX_BASE - path.len() as i64 - (start as i64 * 2));
    }

    if let Some((start, end)) =
        first_literal_match(path, query.pattern, query.case_sensitive, query.whole_word)
    {
        let nucleo_score = nucleo_fuzzy_score(path, query, state).unwrap_or_default();
        return Some(
            PATH_SUBSTRING_BASE + nucleo_score - path.len() as i64 - (start as i64 * 2)
                + if end == path.len() { 4 } else { 0 },
        );
    }

    nucleo_fuzzy_score(path, query, state)
}

pub(super) fn nucleo_fuzzy_score(
    path: &str,
    query: PathQuery<'_>,
    state: &mut PathMatcherState,
) -> Option<i64> {
    let pattern = query.fuzzy_pattern?;
    state.utf32_buf.clear();
    let haystack = Utf32Str::new(path, &mut state.utf32_buf);
    if query.whole_word {
        state.indices.clear();
        let score = pattern.indices(haystack, &mut state.matcher, &mut state.indices)?;
        fuzzy_indices_respect_whole_word(path, &state.indices).then_some(score as i64)
    } else {
        pattern
            .score(haystack, &mut state.matcher)
            .map(|score| score as i64)
    }
}

pub(super) fn fuzzy_indices_respect_whole_word(path: &str, indices: &[u32]) -> bool {
    let Some(first) = indices.iter().min().copied() else {
        return false;
    };
    let Some(last) = indices.iter().max().copied() else {
        return false;
    };
    let chars: Vec<char> = path.chars().collect();
    let first = first as usize;
    let last = last as usize;
    (first == 0 || !is_word_char(chars[first - 1]))
        && (last + 1 >= chars.len() || !is_word_char(chars[last + 1]))
}

pub(super) fn nucleo_case_matching(case_sensitive: bool) -> CaseMatching {
    if case_sensitive {
        CaseMatching::Respect
    } else {
        CaseMatching::Ignore
    }
}
