//! Curated xAI model catalog.
//!
//! `/v1/models` under-reports what the SuperGrok subscription actually accepts
//! — it omits `grok-4-fast` and the Composer models, all verified working — so
//! this curated table is merged into `xai_models`, letting callers (and the
//! agent) see the full known set without memorizing ids.

use serde_json::{Value, json};

/// `(model_id, kind)`; `kind` is `"chat"`, `"image"`, or `"video"`.
pub(super) const CURATED_MODELS: &[(&str, &str)] = &[
    // text — reported by /v1/models
    ("grok-build-0.1", "chat"),
    ("grok-4.3", "chat"),
    ("grok-4.20-0309-reasoning", "chat"),
    ("grok-4.20-0309-non-reasoning", "chat"),
    ("grok-4.20-multi-agent-0309", "chat"),
    // text — accepted by the subscription but absent from /v1/models (verified live)
    ("grok-4-fast", "chat"),
    ("grok-composer-2.5-fast", "chat"),
    ("composer-2.5", "chat"),
    // image / video — Grok Imagine
    ("grok-imagine-image", "image"),
    ("grok-imagine-image-quality", "image"),
    ("grok-imagine-video", "video"),
    ("grok-imagine-video-1.5", "video"),
];

/// Merge the curated catalog with a live `/v1/models` body into a sorted,
/// deduped list of `{ id, kind, live }`. Curated-only ids carry `live: false`;
/// live ids not in the catalog appear with `kind: "unknown"`.
pub(super) fn merge_with_live(live: &Value) -> Vec<Value> {
    use std::collections::BTreeMap;
    let mut by_id: BTreeMap<String, (String, bool)> = CURATED_MODELS
        .iter()
        .map(|(id, kind)| ((*id).to_string(), ((*kind).to_string(), false)))
        .collect();
    for id in live_model_ids(live) {
        by_id
            .entry(id)
            .and_modify(|entry| entry.1 = true)
            .or_insert(("unknown".to_string(), true));
    }
    by_id
        .into_iter()
        .map(|(id, (kind, live))| json!({ "id": id, "kind": kind, "live": live }))
        .collect()
}

/// Extract model ids from a `/v1/models` body, tolerating both `{data:[...]}`
/// and `{models:[...]}` shapes.
fn live_model_ids(live: &Value) -> Vec<String> {
    live.get("data")
        .or_else(|| live.get("models"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_marks_live_and_keeps_curated_extras() {
        let live = json!({ "data": [ { "id": "grok-build-0.1" }, { "id": "grok-new-unlisted" } ] });
        let merged = merge_with_live(&live);
        let find = |id: &str| merged.iter().find(|model| model["id"] == id).cloned();

        let build = find("grok-build-0.1").expect("grok-build-0.1 present");
        assert_eq!(build["live"], json!(true));
        assert_eq!(build["kind"], json!("chat"));

        let composer = find("grok-composer-2.5-fast").expect("curated extra present");
        assert_eq!(composer["live"], json!(false));

        let unlisted = find("grok-new-unlisted").expect("live-only id surfaced");
        assert_eq!(unlisted["live"], json!(true));
        assert_eq!(unlisted["kind"], json!("unknown"));
    }

    #[test]
    fn merge_tolerates_missing_live_body() {
        let merged = merge_with_live(&Value::Null);
        assert_eq!(merged.len(), CURATED_MODELS.len());
        assert!(merged.iter().all(|model| model["live"] == json!(false)));
    }
}
