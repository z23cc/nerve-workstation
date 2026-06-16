use super::*;

pub(super) fn rrf_fuse(dense: &[(usize, f64)], bm25: &[(usize, f64)]) -> Vec<(usize, f64)> {
    let mut scores: HashMap<usize, f64> = HashMap::new();
    for ranking in [dense, bm25] {
        for (rank, (idx, _)) in ranking.iter().enumerate() {
            *scores.entry(*idx).or_insert(0.0) += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    scores.into_iter().collect()
}

pub(super) fn rank_cmp(left: f64, right: f64) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

pub(super) fn chunk_cmp(left: &SemanticChunk, right: &SemanticChunk) -> Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.line_start.cmp(&right.line_start))
        .then_with(|| left.id.cmp(&right.id))
}

pub(super) fn chunk_to_result(chunk: &SemanticChunk, score: f64) -> SemanticSearchResult {
    SemanticSearchResult {
        root_id: chunk.root_id.clone(),
        path: chunk.path.clone(),
        display_path: chunk.display_path.clone(),
        score,
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        symbol: chunk.symbol.clone(),
        signature: chunk.signature.clone(),
        snippet: chunk.text.clone(),
    }
}

pub(super) fn stable_bucket(token: &str, dimension: usize) -> usize {
    let mut hash = 1469598103934665603u64;
    for byte in token.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    (hash as usize) % dimension
}

pub(super) fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}
