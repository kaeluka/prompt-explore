//! Shared attribute validation and deterministic content identities for frontier
//! grouping. Attributes are descriptive metadata; these helpers deliberately do not
//! turn them into a registry or a storage policy.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::llm::ThinkingLevel;
use crate::model::input::{Budget, PromptUnderTest};
use crate::simulate::Workspace;

/// Attributes the harness derives from an investigation and therefore callers must
/// not edit through a metadata PATCH. `label` is the explicit editable display
/// attribute; other valid names are caller-owned arbitrary attributes.
pub const IMMUTABLE_ATTRIBUTE_NAMES: &[&str] = &[
    "put_model",
    "sim_model",
    "put_thinking",
    "sim_thinking",
    "prompt_hash",
    "workspace_hash",
    "scenario_id",
    "scenario_revision",
    "scenario_hash",
    "simulation_backend",
    "step_budget",
    "token_budget",
];

pub fn valid_attribute_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.count() <= 63
        && name[1..]
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn is_immutable_attribute(name: &str) -> bool {
    IMMUTABLE_ATTRIBUTE_NAMES.contains(&name)
}
pub fn is_editable_attribute(name: &str) -> bool {
    name == "label" || (valid_attribute_name(name) && !is_immutable_attribute(name))
}

/// Maximum UTF-8 bytes in one caller-owned attribute value (creation or PATCH).
pub const MAX_ATTRIBUTE_VALUE_BYTES: usize = 1024;

fn validate_edits<'a>(
    entries: impl Iterator<Item = (&'a String, Option<&'a str>)>,
) -> Result<(), String> {
    let mut problems = Vec::new();
    for (key, value) in entries {
        if is_immutable_attribute(key) {
            problems.push(format!("'{key}' is system-owned immutable provenance"));
        } else if !valid_attribute_name(key) {
            problems.push(format!("'{key}' fails ^[a-z][a-z0-9_]{{0,63}}$"));
        }
        if value.is_some_and(|v| v.len() > MAX_ATTRIBUTE_VALUE_BYTES) {
            problems.push(format!(
                "value for '{key}' exceeds {MAX_ATTRIBUTE_VALUE_BYTES} UTF-8 bytes"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("invalid attributes: {}", problems.join("; ")))
    }
}

/// Validate caller-supplied attributes at creation; system keys may not be supplied,
/// even with the same value. Empty strings are valid, distinct from absence.
pub fn validate_post_attributes(attributes: &BTreeMap<String, String>) -> Result<(), String> {
    validate_edits(
        attributes
            .iter()
            .map(|(key, value)| (key, Some(value.as_str()))),
    )
}

/// Validate a complete attribute PATCH before applying any edit. Null deletes an
/// editable key, never a provenance key. The HTTP adapter merges only on success.
pub fn validate_attribute_patch(
    attributes: &BTreeMap<String, Option<String>>,
) -> Result<(), String> {
    validate_edits(
        attributes
            .iter()
            .map(|(key, value)| (key, value.as_deref())),
    )
}

fn thinking_attribute(level: Option<ThinkingLevel>) -> String {
    level
        .map(|level| serde_json::to_value(level).expect("thinking level serializes"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "provider_default".into())
}

/// Assemble authoritative provenance from resolved settings and seed content.
/// Callers validate custom attributes before invoking this; derived values always win.
pub fn system_attributes(
    put_model: &str,
    sim_model: &str,
    put_thinking: Option<ThinkingLevel>,
    sim_thinking: Option<ThinkingLevel>,
    put: &PromptUnderTest,
    workspace_hash: &str,
    custom: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut attributes = custom;
    attributes.insert("put_model".into(), put_model.into());
    attributes.insert("sim_model".into(), sim_model.into());
    attributes.insert("put_thinking".into(), thinking_attribute(put_thinking));
    attributes.insert("sim_thinking".into(), thinking_attribute(sim_thinking));
    attributes.insert("prompt_hash".into(), prompt_hash(put));
    attributes.insert("workspace_hash".into(), workspace_hash.into());
    attributes
}

/// Add execution controls useful for explicit, comparable backend/budget groups.
/// These are provenance, not instructions to infer an experimental cohort.
pub fn with_execution_attributes(
    mut attributes: BTreeMap<String, String>,
    lua_enabled: bool,
    budget: &Budget,
) -> BTreeMap<String, String> {
    attributes.insert(
        "simulation_backend".into(),
        if lua_enabled { "lua" } else { "llm" }.into(),
    );
    attributes.insert("step_budget".into(), budget.max_steps_per_trace.to_string());
    attributes.insert(
        "token_budget".into(),
        budget
            .max_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unlimited".into()),
    );
    attributes
}

/// SHA-256 of a canonical JSON representation of only prompt behavior. The
/// cosmetic PUT `id` is deliberately absent, so renaming a variant cannot
/// create a different lineage attribute.
pub fn prompt_hash(prompt: &PromptUnderTest) -> String {
    let value = json!({
        "template": prompt.template,
        "tools": prompt.tools,
        "design_goals": prompt.design_goals,
    });
    stable_hash_hex(&canonical_json(&value))
}

/// SHA-256 hex encoding used for stable group ids, colors, and content attributes.
pub fn stable_hash_hex(input: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(input.as_bytes());
    format!("{:x}", hash.finalize())
}

/// Length-delimited canonical attribute encoding, so (`a`, `bc`) cannot collide with
/// (`ab`, `c`) and absent remains distinct from the literal string "null".
pub fn canonical_group_attributes(attributes: &BTreeMap<String, Option<String>>) -> String {
    let mut out = String::new();
    for (key, value) in attributes {
        out.push_str(&format!("{}:{}=", key.len(), key));
        match value {
            Some(value) => out.push_str(&format!("s{}:{};", value.len(), value)),
            None => out.push_str("n;"),
        }
    }
    out
}

/// Deterministic workspace identity over its current path/content inventory;
/// zip member order, timestamps, compression, and other archive metadata are
/// intentionally excluded.
pub fn workspace_hash(workspace: &Workspace) -> String {
    workspace.content_hash()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).expect("string JSON"),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => canonical_object(object),
    }
}

fn canonical_object(object: &Map<String, Value>) -> String {
    let mut entries: Vec<_> = object.iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    format!(
        "{{{}}}",
        entries
            .into_iter()
            .map(|(key, value)| {
                format!(
                    "{}:{}",
                    serde_json::to_string(key).expect("key JSON"),
                    canonical_json(value)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_policy_is_shared_by_create_and_patch() {
        for key in IMMUTABLE_ATTRIBUTE_NAMES {
            let create = [(key.to_string(), "forged".into())].into();
            assert!(validate_post_attributes(&create).is_err());
            assert!(validate_attribute_patch(&[(key.to_string(), None)].into()).is_err());
        }
        assert!(validate_post_attributes(&[("empty".into(), "".into())].into()).is_ok());
        assert!(validate_attribute_patch(&[("label".into(), None)].into()).is_ok());
        let maximum = "ü".repeat(MAX_ATTRIBUTE_VALUE_BYTES / 2);
        assert!(validate_post_attributes(&[("label".into(), maximum.clone())].into()).is_ok());
        assert!(
            validate_post_attributes(&[("label".into(), format!("{maximum}x"))].into()).is_err()
        );
        assert!(
            validate_attribute_patch(&[("label".into(), Some(format!("{maximum}x")))].into())
                .is_err()
        );
        assert!(validate_post_attributes(&[("Bad Key".into(), "v".into())].into()).is_err());
    }

    #[test]
    fn canonical_json_is_recursive_order_independent() {
        let left: Value =
            serde_json::from_str(r#"{"z":{"b":2,"a":[{"y":1,"x":0}]},"a":true}"#).unwrap();
        let right: Value =
            serde_json::from_str(r#"{"a":true,"z":{"a":[{"x":0,"y":1}],"b":2}}"#).unwrap();
        assert_eq!(canonical_json(&left), canonical_json(&right));
        assert_eq!(
            stable_hash_hex(&canonical_json(&left)),
            stable_hash_hex(&canonical_json(&right))
        );
    }

    #[test]
    fn canonical_group_attributes_distinguishes_missing_from_literal_null() {
        let missing = [(String::from("x"), None)].into_iter().collect();
        let literal = [(String::from("x"), Some(String::from("null")))]
            .into_iter()
            .collect();
        assert_ne!(
            canonical_group_attributes(&missing),
            canonical_group_attributes(&literal)
        );
    }
}
