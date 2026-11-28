use std::collections::HashSet;

use serde_json::Value;

use crate::{CliArgument, CliOperation, ResourceError, Result};

fn invalid() -> ResourceError {
    ResourceError::before("invalid_call", "CLI 操作或参数不符合登记的 Schema")
}

pub(crate) fn validate_operations(operations: &[CliOperation]) -> Result<()> {
    if operations.is_empty() || operations.len() > 256 {
        return Err(invalid());
    }
    let mut names = HashSet::new();
    for operation in operations {
        if operation.name.is_empty()
            || operation.name.len() > 128
            || !names.insert(&operation.name)
            || operation.args.len() > 64
            || operation
                .executable
                .as_ref()
                .is_some_and(|path| !crate::manifest::safe_relative(path))
            || operation.input_schema.get("type").and_then(Value::as_str) != Some("object")
            || contains_reference(&operation.input_schema)
            || jsonschema::validator_for(&operation.input_schema).is_err()
        {
            return Err(invalid());
        }
        for argument in &operation.args {
            match argument {
                CliArgument::Literal { value } if value.len() <= 8192 && !value.contains('\0') => {}
                CliArgument::Parameter { name }
                    if !name.is_empty()
                        && operation
                            .input_schema
                            .get("properties")
                            .and_then(|value| value.get(name))
                            .is_some() => {}
                _ => return Err(invalid()),
            }
        }
    }
    Ok(())
}

fn contains_reference(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(key.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef")
                || contains_reference(value)
        }),
        Value::Array(values) => values.iter().any(contains_reference),
        _ => false,
    }
}

pub(crate) fn arguments(operation: &CliOperation, value: &Value) -> Result<Vec<String>> {
    let validator = jsonschema::validator_for(&operation.input_schema).map_err(|_| invalid())?;
    if !validator.is_valid(value) {
        return Err(invalid());
    }
    operation
        .args
        .iter()
        .map(|argument| {
            let text = match argument {
                CliArgument::Literal { value } => value.clone(),
                CliArgument::Parameter { name } => match value.get(name).ok_or_else(invalid)? {
                    Value::String(text) => text.clone(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) => value.to_string(),
                    _ => return Err(invalid()),
                },
            };
            if text.contains('\0') || text.len() > 256 * 1024 {
                return Err(invalid());
            }
            Ok(text)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operation() -> CliOperation {
        serde_json::from_value(json!({"name":"echo","description":"回显","input_schema":{
            "type":"object","properties":{"text":{"type":"string","minLength":1}},
            "required":["text"],"additionalProperties":false},
            "args":[{"kind":"literal","value":"--"},{"kind":"parameter","name":"text"}]}))
        .unwrap()
    }

    #[test]
    fn schema_checks_types_unknown_fields_and_keeps_arguments_separate() {
        let operation = operation();
        assert!(validate_operations(std::slice::from_ref(&operation)).is_ok());
        assert_eq!(
            arguments(&operation, &json!({"text":"a b;$(touch x)"})).unwrap(),
            vec!["--", "a b;$(touch x)"]
        );
        for value in [
            json!({}),
            json!({"text":1}),
            json!({"text":""}),
            json!({"text":"x","other":true}),
            json!({"text":"\u{0000}"}),
        ] {
            assert!(arguments(&operation, &value).is_err());
        }
    }

    #[test]
    fn schema_never_resolves_external_references() {
        let mut operation = operation();
        operation.input_schema["properties"]["text"] =
            json!({"$ref":"https://example.test/schema"});
        assert!(validate_operations(&[operation]).is_err());
    }
}
