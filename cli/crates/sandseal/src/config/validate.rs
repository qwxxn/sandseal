use anyhow::{bail, Context, Result};
use jsonschema::Validator;
use serde_json::Value;
use std::path::Path;

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

    Ok(value)
}
