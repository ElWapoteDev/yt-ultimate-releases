use crate::{PluginError, Result};
use serde_json::Value;

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::Invalid(message.into())
}

/// Deliberately bounded JSON Schema subset. Unknown assertion keywords are
/// rejected rather than silently pretending to validate them.
pub(crate) fn check_schema(schema: &Value, depth: usize) -> Result<()> {
    if depth > 8 {
        return Err(invalid("Configuration schema nesting exceeds 8 levels"));
    }
    let object = schema
        .as_object()
        .ok_or_else(|| invalid("Schema must be an object"))?;
    for key in object.keys() {
        if ![
            "type",
            "properties",
            "required",
            "additionalProperties",
            "items",
            "enum",
            "default",
            "title",
            "description",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "$schema",
        ]
        .contains(&key.as_str())
        {
            return Err(invalid(format!("Unsupported schema keyword: {key}")));
        }
    }
    let kind = schema["type"]
        .as_str()
        .ok_or_else(|| invalid("Schema must declare one type"))?;
    if ![
        "object", "array", "string", "integer", "number", "boolean", "null",
    ]
    .contains(&kind)
    {
        return Err(invalid("Unsupported schema type"));
    }
    if let Some(values) = schema.get("enum") {
        if !values
            .as_array()
            .is_some_and(|v| !v.is_empty() && v.len() <= 128)
        {
            return Err(invalid("Schema enum requires 1–128 values"));
        }
    }
    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if let Some(value) = schema.get(key) {
            if value.as_u64().is_none() {
                return Err(invalid(format!("{key} must be a nonnegative integer")));
            }
        }
    }
    for key in ["minimum", "maximum"] {
        if let Some(value) = schema.get(key) {
            if value.as_f64().is_none() {
                return Err(invalid(format!("{key} must be numeric")));
            }
        }
    }
    if let Some(properties) = schema.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| invalid("Schema properties must be an object"))?;
        if kind != "object" || properties.len() > 128 {
            return Err(invalid(
                "Too many schema properties or invalid object schema",
            ));
        }
        for (key, child) in properties {
            if key.len() > 128 {
                return Err(invalid("Schema property name is too long"));
            }
            check_schema(child, depth + 1)?;
        }
    }
    if let Some(required) = schema.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| invalid("Schema required must be an array"))?;
        if required.len() > 128
            || required.iter().any(|v| {
                v.as_str()
                    .is_none_or(|name| schema["properties"].get(name).is_none())
            })
        {
            return Err(invalid("Required fields must name declared properties"));
        }
    }
    if schema
        .get("additionalProperties")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid("additionalProperties must be boolean"));
    }
    if kind == "array" {
        check_schema(
            schema
                .get("items")
                .ok_or_else(|| invalid("Arrays must declare items"))?,
            depth + 1,
        )?;
    }
    Ok(())
}

pub(crate) fn validate(schema: &Value, value: &Value, label: &str) -> Result<()> {
    validate_inner(schema, value, label, 0)
}

fn validate_inner(schema: &Value, value: &Value, label: &str, depth: usize) -> Result<()> {
    if depth > 8 {
        return Err(invalid("Configuration nesting exceeds 8 levels"));
    }
    let kind = schema["type"].as_str().unwrap_or("");
    let correct = match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        _ => false,
    };
    if !correct {
        return Err(invalid(format!("{label} must be {kind}")));
    }
    if let Some(allowed) = schema["enum"].as_array() {
        if !allowed.contains(value) {
            return Err(invalid(format!("{label} is not an allowed value")));
        }
    }
    if let Some(number) = value.as_f64() {
        if schema["minimum"]
            .as_f64()
            .is_some_and(|minimum| number < minimum)
            || schema["maximum"]
                .as_f64()
                .is_some_and(|maximum| number > maximum)
        {
            return Err(invalid(format!("{label} is outside its permitted range")));
        }
    }
    if let Some(text) = value.as_str() {
        let len = text.chars().count() as u64;
        if schema["minLength"]
            .as_u64()
            .is_some_and(|minimum| len < minimum)
            || schema["maxLength"]
                .as_u64()
                .is_some_and(|maximum| len > maximum)
        {
            return Err(invalid(format!("{label} has an invalid text length")));
        }
    }
    if let Some(object) = value.as_object() {
        if object.len() > 128 {
            return Err(invalid(format!("{label} has too many properties")));
        }
        if let Some(required) = schema["required"].as_array() {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(invalid(format!("{label}.{name} is required")));
                }
            }
        }
        for (name, child) in object {
            if let Some(child_schema) = schema["properties"].get(name) {
                validate_inner(child_schema, child, &format!("{label}.{name}"), depth + 1)?;
            } else if schema["additionalProperties"].as_bool() == Some(false) {
                return Err(invalid(format!("Unknown property: {label}.{name}")));
            }
        }
    }
    if let Some(array) = value.as_array() {
        let len = array.len() as u64;
        if len > 1024
            || schema["minItems"]
                .as_u64()
                .is_some_and(|minimum| len < minimum)
            || schema["maxItems"]
                .as_u64()
                .is_some_and(|maximum| len > maximum)
        {
            return Err(invalid(format!("{label} has an invalid number of items")));
        }
        for (index, item) in array.iter().enumerate() {
            validate_inner(
                &schema["items"],
                item,
                &format!("{label}[{index}]"),
                depth + 1,
            )?;
        }
    }
    Ok(())
}
