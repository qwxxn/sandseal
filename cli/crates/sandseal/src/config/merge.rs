use serde_json::Value;

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
}
