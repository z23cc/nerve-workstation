use serde_json::{Value, json};

pub(super) const CURATED_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "gpt-5.3-codex",
];

#[derive(Debug, Clone, Eq, PartialEq)]
struct LiveModel {
    id: String,
    priority: i64,
}

pub(super) fn merge_with_live(live: &Value) -> Vec<Value> {
    let live_ids = live_model_ids(live);
    let mut seen = std::collections::BTreeSet::new();
    let mut merged = Vec::new();

    for id in live_ids {
        if seen.insert(id.clone()) {
            merged.push(json!({ "id": id, "live": true }));
        }
    }
    for id in CURATED_MODELS {
        if seen.insert((*id).to_string()) {
            merged.push(json!({ "id": id, "live": false }));
        }
    }
    merged
}

fn live_model_ids(live: &Value) -> Vec<String> {
    let mut models: Vec<LiveModel> = live
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| !is_hidden(item))
        .filter_map(|item| {
            let id = item.get("slug")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(LiveModel {
                id: id.to_string(),
                priority: item
                    .get("priority")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX),
            })
        })
        .collect();
    models.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    models.into_iter().map(|model| model.id).collect()
}

fn is_hidden(item: &Value) -> bool {
    item.get("visibility")
        .and_then(Value::as_str)
        .map(|visibility| {
            let visibility = visibility.to_ascii_lowercase();
            visibility == "hide" || visibility == "hidden"
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_parses_sorts_filters_and_keeps_curated_fallback() {
        let live = json!({
            "models": [
                { "slug": "z-live", "visibility": "visible", "priority": 20 },
                { "slug": "hidden-live", "visibility": "hidden", "priority": 1 },
                { "slug": "gpt-5.5", "visibility": "show", "priority": 2 },
                { "slug": "a-live", "priority": 20 }
            ]
        });
        let merged = merge_with_live(&live);
        let ids: Vec<_> = merged
            .iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect();
        assert_eq!(&ids[..3], &["gpt-5.5", "a-live", "z-live"]);
        assert!(!ids.contains(&"hidden-live"));
        assert!(ids.contains(&"gpt-5.4"));
        assert_eq!(merged[0]["live"], json!(true));
        let fallback = merged
            .iter()
            .find(|model| model["id"] == "gpt-5.4")
            .unwrap();
        assert_eq!(fallback["live"], json!(false));
    }

    #[test]
    fn merge_tolerates_missing_live_body() {
        let merged = merge_with_live(&Value::Null);
        assert_eq!(merged.len(), CURATED_MODELS.len());
        assert!(merged.iter().all(|model| model["live"] == json!(false)));
    }
}
