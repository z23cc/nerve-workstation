use super::*;

#[derive(Debug, Clone)]
pub(super) struct ChunkBm25 {
    pub(super) docs: Vec<ChunkBm25Doc>,
    pub(super) document_frequencies: HashMap<String, usize>,
    pub(super) avg_doc_len: f64,
}

#[derive(Debug, Clone)]
pub(super) struct ChunkBm25Doc {
    pub(super) chunk_idx: usize,
    pub(super) doc_len: usize,
    pub(super) term_frequencies: HashMap<String, usize>,
}

impl ChunkBm25 {
    pub(super) fn new(chunks: &[SemanticChunk]) -> Self {
        let mut docs = Vec::with_capacity(chunks.len());
        let mut document_frequencies: HashMap<String, usize> = HashMap::new();
        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            let mut term_frequencies = HashMap::new();
            for token in tokenize_text(&chunk.text, false) {
                *term_frequencies.entry(token).or_insert(0) += 1;
            }
            for term in term_frequencies.keys() {
                *document_frequencies.entry(term.clone()).or_insert(0) += 1;
            }
            docs.push(ChunkBm25Doc {
                chunk_idx,
                doc_len: term_frequencies.values().sum::<usize>().max(1),
                term_frequencies,
            });
        }
        let avg_doc_len = if docs.is_empty() {
            1.0
        } else {
            docs.iter().map(|doc| doc.doc_len as f64).sum::<f64>() / docs.len() as f64
        };
        Self {
            docs,
            document_frequencies,
            avg_doc_len,
        }
    }

    pub(super) fn search(&self, query: &str, limit: usize) -> Vec<(usize, f64)> {
        let terms = tokenize_query(query, false);
        if terms.is_empty() || self.docs.is_empty() {
            return Vec::new();
        }
        let doc_count = self.docs.len() as f64;
        let mut scores: Vec<(usize, f64)> = self
            .docs
            .iter()
            .filter_map(|doc| {
                let mut score = 0.0;
                for term in &terms {
                    let tf = doc.term_frequencies.get(term).copied().unwrap_or(0) as f64;
                    if tf == 0.0 {
                        continue;
                    }
                    let df = self.document_frequencies.get(term).copied().unwrap_or(0) as f64;
                    let idf = (1.0 + (doc_count - df + 0.5) / (df + 0.5)).ln();
                    let length_norm = 1.0 - 0.75 + 0.75 * (doc.doc_len as f64 / self.avg_doc_len);
                    let saturated_tf = (tf * (1.2 + 1.0)) / (tf + 1.2 * length_norm);
                    score += idf * saturated_tf;
                }
                (score > 0.0).then_some((doc.chunk_idx, score))
            })
            .collect();
        scores.sort_by(|left, right| rank_cmp(left.1, right.1).then_with(|| left.0.cmp(&right.0)));
        scores.truncate(limit);
        scores
    }
}
