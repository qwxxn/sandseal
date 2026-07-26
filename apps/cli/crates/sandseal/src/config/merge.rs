use serde_json::Value;

/// Directive key: paths this layer replaces wholesale instead of merging.
pub const REPLACE_KEY: &str = "$replace";

/// Merge one settings layer onto the accumulated lower layers.
///
/// A layer may declare `"$replace": ["environment", "files.include"]`. Those paths are
/// dropped from the accumulator before merging, so the layer's value wins whole. Without
/// it, `deep_merge` can only add or overwrite individual keys — never remove one — which
/// is the wrong default for a profile whose job is to take inherited secrets away.
///
/// The directive is stripped from the result; it configures the merge, it is not a setting.
pub fn merge_layer(acc: &Value, layer: &Value) -> Value {
    let mut base = acc.clone();

    for path in replace_paths(layer) {
        remove_path(&mut base, &path);
    }

    let mut merged = deep_merge(&base, layer);
    if let Some(obj) = merged.as_object_mut() {
        obj.remove(REPLACE_KEY);
    }
    merged
}

fn replace_paths(layer: &Value) -> Vec<String> {
    layer
        .get(REPLACE_KEY)
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Remove a dot-separated path from a JSON object tree. A missing path is a no-op.
/// Only object keys are addressable — dots inside user-defined keys (map entries under
/// `files.include`, `environment`) cannot be targeted individually; replace the map instead.
fn remove_path(value: &mut Value, path: &str) {
    let mut segments = path.split('.').peekable();
    let mut cursor = value;

    while let Some(segment) = segments.next() {
        let Some(obj) = cursor.as_object_mut() else {
            return;
        };
        if segments.peek().is_none() {
            obj.remove(segment);
            return;
        }
        match obj.get_mut(segment) {
            Some(next) => cursor = next,
            None => return,
        }
    }
}

/// Deep merge two JSON values:
/// - Objects: recursively merged, `b` wins for scalar key conflicts
/// - Arrays: concatenated (a first, then b), deduplicated preserving insertion order
/// - Scalars: `b` wins
pub fn deep_merge(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Object(map_a), Value::Object(map_b)) => {
            let mut merged = map_a.clone();
            for (key, val_b) in map_b {
                let val = match merged.get(key) {
                    Some(val_a) => deep_merge(val_a, val_b),
                    None => val_b.clone(),
                };
                merged.insert(key.clone(), val);
            }
            Value::Object(merged)
        }
        (Value::Array(arr_a), Value::Array(arr_b)) => {
            let mut seen = Vec::new();
            let mut result = Vec::new();
            for item in arr_a.iter().chain(arr_b.iter()) {
                let serialized = item.to_string();
                if !seen.contains(&serialized) {
                    seen.push(serialized);
                    result.push(item.clone());
                }
            }
            Value::Array(result)
        }
        (_, b) => b.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_objects_recursively() {
        let a = json!({"files": {"exclude": [".env"]}, "environment": {"A": "1"}});
        let b = json!({"files": {"exclude": ["dist"]}, "environment": {"B": "2"}});
        let result = deep_merge(&a, &b);

        assert_eq!(
            result["files"]["exclude"],
            json!([".env", "dist"])
        );
        assert_eq!(result["environment"]["A"], json!("1"));
        assert_eq!(result["environment"]["B"], json!("2"));
    }

    #[test]
    fn merge_arrays_dedup() {
        let a = json!([".env", ".env.local"]);
        let b = json!([".env", "dist"]);
        let result = deep_merge(&a, &b);
        assert_eq!(result, json!([".env", ".env.local", "dist"]));
    }

    #[test]
    fn scalar_b_wins() {
        let a = json!("old");
        let b = json!("new");
        assert_eq!(deep_merge(&a, &b), json!("new"));
    }

    #[test]
    fn nested_object_merge() {
        let a = json!({"hooks": {"setup": {"script": "a.sh"}}});
        let b = json!({"hooks": {"prestart": [{"script": "b.sh"}]}});
        let result = deep_merge(&a, &b);

        assert_eq!(result["hooks"]["setup"]["script"], json!("a.sh"));
        assert_eq!(result["hooks"]["prestart"][0]["script"], json!("b.sh"));
    }

    #[test]
    fn empty_merge() {
        let a = json!({});
        let b = json!({"files": {"exclude": [".env"]}});
        let result = deep_merge(&a, &b);
        assert_eq!(result["files"]["exclude"], json!([".env"]));
    }

    #[test]
    fn replace_drops_inherited_object_keys() {
        let acc = json!({"environment": {"API_TOKEN": "inherited", "DEPLOY_KEY": "inherited"}});
        let layer = json!({"$replace": ["environment"], "environment": {"CI": "1"}});

        let result = merge_layer(&acc, &layer);

        assert_eq!(result["environment"], json!({"CI": "1"}));
    }

    #[test]
    fn replace_with_empty_object_clears_everything() {
        let acc = json!({"environment": {"API_TOKEN": "inherited"}});
        let layer = json!({"$replace": ["environment"], "environment": {}});

        assert_eq!(merge_layer(&acc, &layer)["environment"], json!({}));
    }

    #[test]
    fn replace_overrides_array_concatenation() {
        let acc = json!({"dependencies": ["jq", "curl"]});
        let layer = json!({"$replace": ["dependencies"], "dependencies": ["ripgrep"]});

        assert_eq!(merge_layer(&acc, &layer)["dependencies"], json!(["ripgrep"]));
    }

    #[test]
    fn replace_targets_nested_paths_only() {
        let acc = json!({
            "files": {
                "exclude": [".env"],
                "include": {"/home/user/.config/creds": "/home/agent/.config/creds"}
            }
        });
        let layer = json!({"$replace": ["files.include"], "files": {"exclude": ["dist"]}});

        let result = merge_layer(&acc, &layer);

        // The targeted path is gone...
        assert!(result["files"].get("include").is_none());
        // ...its sibling still merges normally.
        assert_eq!(result["files"]["exclude"], json!([".env", "dist"]));
    }

    #[test]
    fn replace_directive_is_stripped_from_output() {
        let layer = json!({"$replace": ["environment"], "environment": {}});
        let result = merge_layer(&json!({}), &layer);

        assert!(result.get("$replace").is_none());
    }

    #[test]
    fn replace_of_missing_path_is_a_noop() {
        let acc = json!({"network": {"mode": "host"}});
        let layer = json!({"$replace": ["environment", "files.include"]});

        assert_eq!(merge_layer(&acc, &layer), json!({"network": {"mode": "host"}}));
    }

    #[test]
    fn without_directive_merge_layer_matches_deep_merge() {
        let acc = json!({"environment": {"A": "1"}, "files": {"exclude": [".env"]}});
        let layer = json!({"environment": {"B": "2"}, "files": {"exclude": ["dist"]}});

        assert_eq!(merge_layer(&acc, &layer), deep_merge(&acc, &layer));
    }
}
