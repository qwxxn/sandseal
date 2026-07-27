use anyhow::{bail, Context, Result};
use jsonschema::Validator;
use serde_json::Value;
use std::path::Path;

use crate::config::merge::REPLACE_KEY;

const SCHEMA: &str = include_str!("../../../../schema/settings.schema.json");

pub fn validate_settings(path: &Path) -> Result<Value> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read settings: {}", path.display()))?;

    let value: Value = serde_json::from_str(&content)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;

    let schema: Value = serde_json::from_str(SCHEMA)
        .expect("embedded schema is valid JSON");

    let validator = Validator::new(&schema)
        .expect("embedded schema is valid JSON Schema");

    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("  - {}: {}", e.instance_path, e))
        .collect();

    if !errors.is_empty() {
        bail!(
            "invalid settings in {}:\n{}",
            path.display(),
            errors.join("\n")
        );
    }

    validate_replace_paths(&schema, &value, path)?;

    Ok(value)
}

/// Check that every `$replace` entry addresses a key the schema knows about.
///
/// A path that resolves to nothing silently disables the directive, and `$replace` is what
/// a profile uses to drop inherited secrets — so a typo like `["environment, files"]`
/// (one string, not two) has to fail loudly rather than leave the secrets in place.
fn validate_replace_paths(schema: &Value, value: &Value, path: &Path) -> Result<()> {
    let Some(entries) = value.get(REPLACE_KEY).and_then(Value::as_array) else {
        return Ok(());
    };

    let unknown: Vec<String> = entries
        .iter()
        .filter_map(Value::as_str)
        .filter(|entry| !schema_knows_path(schema, entry))
        .map(|entry| format!("  - unknown path: {entry:?}"))
        .collect();

    if !unknown.is_empty() {
        bail!(
            "invalid \"{}\" in {}:\n{}\n  Entries are dot-separated settings keys, one per \
             array item — e.g. [\"environment\", \"files.include\"].",
            REPLACE_KEY,
            path.display(),
            unknown.join("\n")
        );
    }

    Ok(())
}

/// Walk a dot-separated path through the schema. A free-form map (`additionalProperties`
/// holding a schema) swallows the rest of the path, since its keys are user-defined —
/// that is what makes `environment.SOME_VAR` valid while `files.include./a/b` is not.
fn schema_knows_path(schema: &Value, path: &str) -> bool {
    let mut node = schema;

    for segment in path.split('.') {
        match node.get("properties").and_then(|p| p.get(segment)) {
            Some(next) => node = next,
            None => return node.get("additionalProperties").is_some_and(Value::is_object),
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        serde_json::from_str(SCHEMA).unwrap()
    }

    #[test]
    fn accepts_top_level_and_nested_keys() {
        let s = schema();
        assert!(schema_knows_path(&s, "environment"));
        assert!(schema_knows_path(&s, "dependencies"));
        assert!(schema_knows_path(&s, "files.include"));
        assert!(schema_knows_path(&s, "files.exclude"));
        assert!(schema_knows_path(&s, "docker.passthrough"));
    }

    #[test]
    fn accepts_user_defined_keys_inside_free_form_maps() {
        let s = schema();
        assert!(schema_knows_path(&s, "environment.API_TOKEN"));
        assert!(schema_knows_path(&s, "network.services.db"));
    }

    #[test]
    fn rejects_a_comma_separated_string() {
        // The mistake this check exists for: one string instead of two array items.
        assert!(!schema_knows_path(&schema(), "environment, files"));
    }

    #[test]
    fn rejects_unknown_and_malformed_paths() {
        let s = schema();
        assert!(!schema_knows_path(&s, "envrionment"));
        assert!(!schema_knows_path(&s, "files.exclude.deeper"));
        assert!(!schema_knows_path(&s, "files."));
        assert!(!schema_knows_path(&s, ""));
    }

    fn validate_value(value: Value) -> Result<Value> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        validate_settings(&path)
    }

    #[test]
    fn accepts_a_memory_scope() {
        let value =
            serde_json::json!({"memory": {"scope": {"project": "popitchiweb", "crossProject": false}}});
        assert!(validate_value(value).is_ok());
        assert!(schema_knows_path(&schema(), "memory.scope.project"));
    }

    #[test]
    fn rejects_a_project_name_the_server_would_refuse() {
        // Same pattern the memory service enforces. Catching it here means a bad name fails
        // at `config edit`, not silently at the next session when the scope drops to null.
        let err = validate_value(serde_json::json!({"memory": {"scope": {"project": "not valid!"}}}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("/memory/scope/project"), "{err}");
    }

    #[test]
    fn rejects_the_flat_form_that_never_shipped() {
        // Scope was nested before release. A settings file written against the flat shape must
        // say so rather than parse into a sandbox that silently ignores it.
        assert!(validate_value(serde_json::json!({"memory": {"project": "demo"}})).is_err());
    }

    #[test]
    fn missing_directive_is_fine() {
        let value = serde_json::json!({"network": {"mode": "bridge"}});
        assert!(validate_replace_paths(&schema(), &value, Path::new("x.json")).is_ok());
    }

    #[test]
    fn error_names_every_bad_entry() {
        let value = serde_json::json!({"$replace": ["environment, files", "environment", "nope"]});
        let err = validate_replace_paths(&schema(), &value, Path::new("night.json"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("environment, files"));
        assert!(err.contains("nope"));
        assert!(err.contains("night.json"));
    }
}
