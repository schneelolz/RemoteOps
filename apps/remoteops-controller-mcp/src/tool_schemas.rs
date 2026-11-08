//! 不依赖配置或网络的实际工具目录契约检查。

use std::collections::BTreeSet;

use rmcp::model::Tool;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct SchemaReport {
    tool_count: usize,
    passed: bool,
    failures: Vec<String>,
    tools: Vec<Tool>,
}

pub(super) fn check(tools: Vec<Tool>) -> anyhow::Result<()> {
    let report = inspect(tools);
    println!("{}", serde_json::to_string(&report)?);
    anyhow::ensure!(report.passed, "工具 Schema 自检失败；详见 failures 路径");
    Ok(())
}

pub(super) fn inspect(tools: Vec<Tool>) -> SchemaReport {
    let mut names = BTreeSet::new();
    let mut failures = Vec::new();
    if tools.is_empty() {
        failures.push("/tools: 工具目录不能为空".into());
    }
    for (index, tool) in tools.iter().enumerate() {
        let path = format!("/tools/{index}");
        if tool.name.is_empty()
            || tool.name.len() > 128
            || !tool
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            || !names.insert(tool.name.as_ref())
        {
            failures.push(format!("{path}/name: 工具名称无效或重复"));
        }
        if let Some(annotations) = &tool.annotations {
            for (name, value) in [
                ("readOnlyHint", annotations.read_only_hint),
                ("destructiveHint", annotations.destructive_hint),
                ("idempotentHint", annotations.idempotent_hint),
                ("openWorldHint", annotations.open_world_hint),
            ] {
                if value.is_none() {
                    failures.push(format!("{path}/annotations/{name}: 缺少工具行为声明"));
                }
            }
        } else {
            failures.push(format!("{path}/annotations: 缺少工具行为声明"));
        }
        let input = Value::Object((*tool.input_schema).clone());
        inspect_root(&input, &format!("{path}/inputSchema"), &mut failures);
        if let Some(output) = &tool.output_schema {
            inspect_root(
                &Value::Object((**output).clone()),
                &format!("{path}/outputSchema"),
                &mut failures,
            );
        }
    }
    SchemaReport {
        tool_count: tools.len(),
        passed: failures.is_empty(),
        failures,
        tools,
    }
}

fn inspect_root(schema: &Value, path: &str, failures: &mut Vec<String>) {
    // JSON Schema 本身允许省略 type；MCP 工具契约要求根类型明确为 object。
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        failures.push(format!("{path}/type: MCP 工具 Schema 根类型必须是 object"));
    }
    inspect_schema(schema, schema, path, failures);
}

fn pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn inspect_schema(schema: &Value, root: &Value, path: &str, failures: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        if !schema.is_boolean() {
            failures.push(format!("{path}: Schema 必须是对象或布尔值"));
        }
        return;
    };
    if let Some(kind) = object.get("type") {
        let valid = |value: &Value| {
            matches!(
                value.as_str(),
                Some("null" | "boolean" | "object" | "array" | "number" | "integer" | "string")
            )
        };
        let valid_type = match kind {
            Value::Array(types) => {
                let unique: BTreeSet<_> = types.iter().filter_map(Value::as_str).collect();
                !types.is_empty() && types.iter().all(valid) && unique.len() == types.len()
            }
            value => valid(value),
        };
        if !valid_type {
            failures.push(format!("{path}/type: 非法 JSON Schema 类型"));
        }
    }
    if let Some(reference) = object.get("$ref") {
        // 当前工具只生成本地引用；离线自检不解析外部 URL。
        if !reference.as_str().is_some_and(|value| {
            value == "#"
                || value
                    .strip_prefix('#')
                    .is_some_and(|p| root.pointer(p).is_some())
        }) {
            failures.push(format!("{path}/$ref: 无法解析的本地 Schema 引用"));
        }
    }
    for keyword in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = object.get(keyword) {
            if let Some(children) = children.as_object() {
                for (name, child) in children {
                    // SDK 1.32.2 的 MCP 属性契约要求对象形式，不能用布尔 true 表示任意值。
                    if keyword == "properties" && !child.is_object() {
                        failures.push(format!(
                            "{path}/{keyword}/{}: MCP 属性 Schema 必须是对象",
                            pointer_segment(name)
                        ));
                    }
                    inspect_schema(
                        child,
                        root,
                        &format!("{path}/{keyword}/{}", pointer_segment(name)),
                        failures,
                    );
                }
            } else {
                failures.push(format!("{path}/{keyword}: 必须是 Schema 映射"));
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = object.get(keyword) {
            if let Some(children) = children.as_array().filter(|children| !children.is_empty()) {
                for (index, child) in children.iter().enumerate() {
                    inspect_schema(child, root, &format!("{path}/{keyword}/{index}"), failures);
                }
            } else {
                failures.push(format!("{path}/{keyword}: 必须是非空 Schema 数组"));
            }
        }
    }
    for keyword in [
        "items",
        "contains",
        "additionalProperties",
        "unevaluatedProperties",
        "unevaluatedItems",
        "propertyNames",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(child) = object.get(keyword) {
            inspect_schema(child, root, &format!("{path}/{keyword}"), failures);
        }
    }
    if let Some(required) = object.get("required") {
        let valid = required.as_array().is_some_and(|fields| {
            let unique: BTreeSet<_> = fields.iter().filter_map(Value::as_str).collect();
            unique.len() == fields.len()
        });
        if !valid {
            failures.push(format!("{path}/required: 必须是不重复的字段名称数组"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn actual_default_and_test_tool_routers_pass() {
        for enabled in [false, true] {
            let report = inspect(crate::configured_tool_router(enabled).list_all());
            assert!(report.passed, "{:?}", report.failures);
            assert!(report.tool_count > 20);
            for name in ["manage_relay_tools", "upload_relay_tool_package"] {
                let tool = report.tools.iter().find(|tool| tool.name == name).unwrap();
                assert_eq!(tool.output_schema.as_ref().unwrap()["type"], "object");
                assert_eq!(
                    tool.output_schema.as_ref().unwrap()["properties"]["data"],
                    json!({})
                );
            }
        }
    }

    #[test]
    fn rejects_missing_root_type_invalid_nested_types_and_duplicate_names_at_end() {
        let mut tools = crate::configured_tool_router(false).list_all();
        let last = tools.last_mut().unwrap();
        last.output_schema = Some(std::sync::Arc::new(
            serde_json::from_value(json!({"$schema":
            "https://json-schema.org/draft/2020-12/schema"}))
            .unwrap(),
        ));
        assert!(
            inspect(tools.clone())
                .failures
                .iter()
                .any(|path| path.ends_with("根类型必须是 object"))
        );
        let last = tools.last_mut().unwrap();
        last.output_schema = Some(std::sync::Arc::new(
            serde_json::from_value(json!({"type": "object",
            "properties": {"nested": {"type": "invalid"}}}))
            .unwrap(),
        ));
        assert!(
            inspect(tools.clone())
                .failures
                .iter()
                .any(|path| path.contains("/properties/nested/type"))
        );
        tools.push(tools[0].clone());
        assert!(
            inspect(tools)
                .failures
                .iter()
                .any(|path| path.contains("/name"))
        );
    }

    #[test]
    fn rejects_boolean_properties_but_preserves_valid_boolean_additional_properties() {
        let mut tools = crate::configured_tool_router(false).list_all();
        for value in [true, false] {
            tools.last_mut().unwrap().output_schema = Some(std::sync::Arc::new(
                serde_json::from_value(json!({"type": "object", "properties": {"data": value}}))
                    .unwrap(),
            ));
            let report = inspect(tools.clone());
            assert!(
                report
                    .failures
                    .iter()
                    .any(|path| path.contains("/properties/data"))
            );
        }
        tools.last_mut().unwrap().output_schema = Some(std::sync::Arc::new(
            serde_json::from_value(json!({"type": "object", "additionalProperties": true}))
                .unwrap(),
        ));
        assert!(inspect(tools).passed);
    }
}
