use super::*;

pub(super) struct DenseAnn {
    pub(super) hnsw: Option<Hnsw<'static, f32, DistCosine>>,
}

impl DenseAnn {
    pub(super) fn new(vectors: Vec<Vec<f32>>, dimension: usize) -> Result<Self, NerveError> {
        if vectors.is_empty() {
            return Ok(Self { hnsw: None });
        }
        for vector in &vectors {
            if vector.len() != dimension {
                return Err(NerveError::Semantic(format!(
                    "embedding dimension mismatch: expected {dimension}, got {}",
                    vector.len()
                )));
            }
        }
        let hnsw = Hnsw::<f32, DistCosine>::new(
            HNSW_MAX_CONN,
            vectors.len().max(1),
            HNSW_MAX_LAYER,
            HNSW_EF_CONSTRUCTION,
            DistCosine {},
        );
        for (idx, vector) in vectors.iter().enumerate() {
            hnsw.insert((vector.as_slice(), idx));
        }
        Ok(Self { hnsw: Some(hnsw) })
    }

    pub(super) fn search(&self, query: &[f32], limit: usize) -> Vec<(usize, f64)> {
        let Some(hnsw) = &self.hnsw else {
            return Vec::new();
        };
        hnsw.search(query, limit, HNSW_EF_SEARCH)
            .into_iter()
            .map(|neighbour| (neighbour.d_id, 1.0 / (1.0 + neighbour.distance as f64)))
            .collect()
    }

    pub(super) fn dump(&self, dir: &Path, basename: &str) -> Result<Option<String>, NerveError> {
        let Some(hnsw) = &self.hnsw else {
            return Ok(None);
        };
        hnsw.file_dump(dir, basename)
            .map(Some)
            .map_err(|err| NerveError::Semantic(format!("semantic ANN dump failed: {err}")))
    }
}
