use serde::{Deserialize, Serialize};

use super::{PATH_WEIGHT, REPOMAP_WEIGHT, SEARCH_WEIGHT};

/// Deterministic per-signal contribution trace for build_context ranking.
///
/// Values are fixed-precision strings so structured output stays byte-stable
/// across platforms and serde float formatting changes. The three signal fields
/// are rounded weighted contributions; `total` matches the authoritative
/// ranking score exposed as the entry's existing `score` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildContextScoreBreakdown {
    pub search: String,
    pub repo_map: String,
    pub path: String,
    pub total: String,
    pub source: String,
}

/// One budget trial for a candidate file/mode during greedy allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildContextAllocationAttempt {
    pub mode: String,
    pub total_tokens: usize,
    pub accepted: bool,
}

/// Explainable allocation result for one ranked candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildContextAllocationTrace {
    pub path: String,
    pub display_path: String,
    pub score: String,
    pub score_breakdown: BuildContextScoreBreakdown,
    pub attempts: Vec<BuildContextAllocationAttempt>,
    pub result: String,
    pub reason: String,
}

impl BuildContextScoreBreakdown {
    pub(super) fn from_normalized(search: f64, repo_map: f64, path: f64, total: f64) -> Self {
        Self {
            search: format_score(search * SEARCH_WEIGHT),
            repo_map: format_score(repo_map * REPOMAP_WEIGHT),
            path: format_score(path * PATH_WEIGHT),
            total: format_score(total),
            source: "ranked".to_string(),
        }
    }

    pub fn zero() -> Self {
        Self {
            search: format_score(0.0),
            repo_map: format_score(0.0),
            path: format_score(0.0),
            total: format_score(0.0),
            source: "not_ranked".to_string(),
        }
    }
}

fn format_score(score: f64) -> String {
    format!("{score:.6}")
}
