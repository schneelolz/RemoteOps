//! 从共享模型生成管理契约；诊断仅使用契约中的字段和约束。

use std::{fmt::Write as _, sync::OnceLock};

use jsonschema::{ValidationError, error::ValidationErrorKind};
use remoteops_tool_center::ToolCenterCommand;
use serde_json::{Value, json};

fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(ToolCenterCommand)).unwrap())
}

pub(super) fn parse_command(value: Value) -> Result<ToolCenterCommand, String> {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    let validator = VALIDATOR
        .get_or_init(|| jsonschema::validator_for(schema()).expect("共享工具管理 Schema 必须有效"));
    if let Err(error) = validator.validate(&value) {
        let (field, expected) = diagnose(&error, &value);
        return Err(failure(&field, &expected));
    }
    if let Some((field, expected)) = integer_encoding_error(schema(), &value, "command".into()) {
        return Err(failure(&field, &expected));
    }
    // 领域转换继续保留；不输出 serde 异常及输入内容。
    serde_json::from_value(value).map_err(|_| failure("command", "有效的工具管理命令"))
}

// JSON Schema 接受 1.0 形式的 integer，serde 的整数类型要求整数编码。
// 沿生成契约遍历，补足两者表示差异；不检查调用方自带的 CLI input_schema。
fn integer_encoding_error(node: &Value, value: &Value, field: String) -> Option<(String, String)> {
    let node = active_schema(node, value);
    let integer = node.get("type").is_some_and(|kind| {
        kind == "integer"
            || kind
                .as_array()
                .is_some_and(|types| types.contains(&json!("integer")))
    });
    if integer && value.is_f64() {
        return Some((field, format!("{}；使用整数 JSON 编码", expected(node))));
    }
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            if let Some(value) = value.get(name)
                && let Some(error) = integer_encoding_error(child, value, format!("{field}.{name}"))
            {
                return Some(error);
            }
        }
    }
    if let Some((items, values)) = node.get("items").zip(value.as_array()) {
        for (index, value) in values.iter().enumerate() {
            if let Some(error) = integer_encoding_error(items, value, format!("{field}[{index}]")) {
                return Some(error);
            }
        }
    }
    None
}

fn failure(field: &str, expected: &str) -> String {
    json!({
        "code": "invalid_tool_center_command",
        "message": "工具管理命令不符合输入契约",
        "details": { "field": field, "expected": expected }
    })
    .to_string()
}

fn resolve(mut node: &Value) -> &Value {
    while let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let Some(target) = reference
            .strip_prefix('#')
            .and_then(|p| schema().pointer(p))
        else {
            break;
        };
        node = target;
    }
    node
}

fn branches(node: &Value) -> Option<&Vec<Value>> {
    node.get("oneOf")
        .or_else(|| node.get("anyOf"))
        .and_then(Value::as_array)
}

fn choices(node: &Value) -> Vec<Value> {
    node.get("enum")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| node.get("const").map(|value| vec![value.clone()]))
        .unwrap_or_default()
}

// 标记和值都取自生成的契约；未知 action/kind 不能决定诊断路径。
fn discriminator(variants: &[Value]) -> Option<(&'static str, Vec<Value>)> {
    for tag in ["action", "kind"] {
        let values: Vec<_> = variants
            .iter()
            .flat_map(|variant| choices(&resolve(variant)["properties"][tag]))
            .collect();
        if !values.is_empty() {
            return Some((tag, values));
        }
    }
    None
}

fn selected_branch(variants: &[Value], value: &Value) -> Option<usize> {
    if let Some((tag, _)) = discriminator(variants) {
        return variants.iter().position(|variant| {
            choices(&resolve(variant)["properties"][tag]).contains(&value[tag])
        });
    }
    // Option<T> 的 null 分支和实际类型分开处理，避免返回笼统的 anyOf 错误。
    variants.iter().position(|variant| {
        let node = resolve(variant);
        node.get("type")
            .is_some_and(|kind| type_matches(kind, value))
            || (node.get("type").is_none() && !value.is_null())
    })
}

fn active_schema<'a>(mut node: &'a Value, value: &Value) -> &'a Value {
    loop {
        node = resolve(node);
        let Some(variants) = branches(node) else {
            return node;
        };
        let Some(index) = selected_branch(variants, value) else {
            return node;
        };
        node = &variants[index];
    }
}

fn type_matches(kind: &Value, value: &Value) -> bool {
    if let Some(types) = kind.as_array() {
        return types.iter().any(|kind| type_matches(kind, value));
    }
    match kind.as_str() {
        Some("null") => value.is_null(),
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("number") => value.is_number(),
        _ => false,
    }
}

// 只从已知 properties 或数组索引构造路径，绝不回显任意输入键。
fn locate<'a>(pointer: &str, value: &'a Value) -> (&'static Value, &'a Value, String) {
    let mut node = schema();
    let mut instance = value;
    let mut field = "command".to_owned();
    for part in pointer.split('/').skip(1) {
        node = active_schema(node, instance);
        if let Some((name, child)) = node
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get_key_value(part))
        {
            field.push('.');
            field.push_str(name);
            node = child;
            instance = &instance[name];
        } else if let Some((index, items)) = part
            .parse::<usize>()
            .ok()
            .zip(node.get("items"))
            .filter(|(index, _)| {
                instance
                    .as_array()
                    .is_some_and(|items| *index < items.len())
            })
        {
            let _ = write!(field, "[{index}]");
            node = items;
            instance = &instance[index];
        } else {
            break;
        }
    }
    (resolve(node), instance, field)
}

fn expected(node: &Value) -> String {
    let node = resolve(node);
    let values = choices(node);
    if !values.is_empty() {
        return format!("合法取值 {}", Value::Array(values));
    }
    if let Some(kind) = node.get("type") {
        let mut description = kind
            .as_str()
            .map_or_else(|| kind.to_string(), str::to_owned);
        for keyword in ["minimum", "maximum"] {
            if let Some(limit) = node.get(keyword) {
                let _ = write!(description, "; {keyword}={limit}");
            }
        }
        return description;
    }
    if let Some(variants) = branches(node) {
        return variants
            .iter()
            .map(expected)
            .collect::<Vec<_>>()
            .join(" 或 ");
    }
    "符合字段 Schema 的 JSON 值".to_owned()
}

fn diagnose(error: &ValidationError<'_>, value: &Value) -> (String, String) {
    let (node, instance, field) = locate(error.instance_path.as_str(), value);
    match &error.kind {
        ValidationErrorKind::OneOfNotValid { context } | ValidationErrorKind::AnyOf { context } => {
            if let Some(variants) = branches(node) {
                if let Some(index) = selected_branch(variants, instance) {
                    if let Some(child) = context.get(index).and_then(|errors| errors.first()) {
                        return diagnose(child, value);
                    }
                } else if let Some((tag, values)) = discriminator(variants) {
                    if !instance.is_object() {
                        return (field, "object".to_owned());
                    }
                    return (
                        format!("{field}.{tag}"),
                        format!("合法取值 {}", Value::Array(values)),
                    );
                }
            }
        }
        ValidationErrorKind::Required { property } => {
            let node = active_schema(node, instance);
            if let Some((name, child)) = property
                .as_str()
                .and_then(|name| node.get("properties")?.as_object()?.get_key_value(name))
            {
                return (
                    format!("{field}.{name}"),
                    format!("必填；{}", expected(child)),
                );
            }
        }
        _ => {}
    }
    (field, expected(active_schema(node, instance)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::tool::IntoCallToolResult;

    fn example(name: &str) -> Value {
        let text = match name {
            "external" => include_str!(
                "../../../deploy/skills/remoteops/references/examples/put-tool-external.json"
            ),
            "hosted" => include_str!(
                "../../../deploy/skills/remoteops/references/examples/put-tool-hosted.json"
            ),
            "record" => include_str!(
                "../../../deploy/skills/remoteops/references/examples/record-test.json"
            ),
            _ => unreachable!(),
        };
        serde_json::from_str(text).unwrap()
    }

    fn check_error(command: Value, field: &str, expected: &str) {
        let message = parse_command(command).unwrap_err();
        let error: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(error["code"], "invalid_tool_center_command");
        assert_eq!(error["details"]["field"], field, "{message}");
        assert!(
            error["details"]["expected"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{message}"
        );
        let result: Result<rmcp::Json<crate::ToolCenterOutput>, String> = Err(message);
        let rmcp::model::CallToolResponse::Complete(result) =
            result.into_call_tool_result().unwrap()
        else {
            panic!("本地校验不应请求额外输入");
        };
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn complete_documented_examples_match_actual_input_schema_and_domain_models() {
        let tool = crate::configured_tool_router(false)
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "manage_relay_tools")
            .unwrap();
        let actual_schema = Value::Object((*tool.input_schema).clone());
        let validator = jsonschema::validator_for(&actual_schema).unwrap();
        for name in ["external", "hosted", "record"] {
            let value = example(name);
            assert!(validator.is_valid(&value), "{name}");
            let _: remoteops_tool_center::ToolCenterRequest =
                serde_json::from_value(value.clone()).unwrap();
            assert!(parse_command(value["command"].clone()).is_ok(), "{name}");
        }
    }

    #[test]
    fn discovers_seven_public_actions_and_rejects_all_internal_upload_actions() {
        let variants = schema()["oneOf"].as_array().unwrap();
        let (_, actions) = discriminator(variants).unwrap();
        assert_eq!(
            actions,
            [
                "snapshot",
                "set_policy",
                "set_guide",
                "put_tool",
                "set_tool_enabled",
                "record_test",
                "import_external"
            ]
            .map(Value::from)
        );
        for action in [
            "begin_upload",
            "upload_chunk",
            "finish_upload",
            "cancel_upload",
            "secret-token",
        ] {
            check_error(json!({"action": action}), "command.action", "snapshot");
        }
        for command in [json!({}), json!({"action": null}), json!({"action": 12})] {
            check_error(command, "command.action", "record_test");
        }
        assert!(parse_command(json!({"action":"snapshot"})).is_ok());
        assert!(parse_command(json!({"action":"set_policy", "enabled":false})).is_ok());
        assert!(
            parse_command(
                json!({"action":"set_tool_enabled", "tool_id":"example", "enabled":true})
            )
            .is_ok()
        );
        assert!(
            parse_command(
                json!({"action":"import_external", "url":"https://example.invalid/tool.zip"})
            )
            .is_ok()
        );
    }

    #[test]
    fn missing_fields_and_wrong_types_report_the_selected_branch() {
        for (pointer, field) in [
            ("/tool/name", "command.tool.name"),
            (
                "/tool/versions/0/version",
                "command.tool.versions[0].version",
            ),
            (
                "/tool/versions/0/packages/0/tool/platforms/0/os",
                "command.tool.versions[0].packages[0].tool.platforms[0].os",
            ),
        ] {
            let mut command = example("external")["command"].clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            command
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            check_error(command, field, "string");
        }
        for (pointer, replacement, field, expected) in [
            (
                "/tool/versions",
                json!({}),
                "command.tool.versions",
                "array",
            ),
            (
                "/tool/versions/0/packages/0/tool/startup/args/0",
                json!(12),
                "command.tool.versions[0].packages[0].tool.startup.args[0]",
                "string",
            ),
            (
                "/tool/versions/0/packages/0/tool/package/size",
                json!("secret-token"),
                "command.tool.versions[0].packages[0].tool.package.size",
                "integer",
            ),
            (
                "/tool/versions/0/packages/0/tool/platforms/0/min_windows_build",
                json!({}),
                "command.tool.versions[0].packages[0].tool.platforms[0].min_windows_build",
                "integer",
            ),
        ] {
            let mut command = example("external")["command"].clone();
            if pointer.ends_with("/args/0") {
                command
                    .pointer_mut("/tool/versions/0/packages/0/tool/startup")
                    .unwrap()["args"] = json!(["placeholder"]);
            }
            *command.pointer_mut(pointer).unwrap() = replacement;
            check_error(command, field, expected);
        }
        let mut command = example("record")["command"].clone();
        command["record"] = json!({});
        let error: Value = serde_json::from_str(&parse_command(command).unwrap_err()).unwrap();
        assert!(
            error["details"]["field"]
                .as_str()
                .unwrap()
                .starts_with("command.record.")
        );
        check_error(
            json!({"action":"record_test", "tool_id":"x", "version":"1", "sha256":"x", "record":null}),
            "command.record",
            "object",
        );
        check_error(
            json!({"action":"set_policy", "enabled":"false"}),
            "command.enabled",
            "boolean",
        );
    }

    #[test]
    fn enums_and_sources_report_legal_values_without_echoing_secrets() {
        for (pointer, field, expected) in [
            (
                "/tool/versions/0/packages/0/tool/startup/interface",
                "command.tool.versions[0].packages[0].tool.startup.interface",
                "stdio_mcp",
            ),
            (
                "/tool/versions/0/packages/0/tool/package/format",
                "command.tool.versions[0].packages[0].tool.package.format",
                "zip",
            ),
            (
                "/tool/versions/0/packages/0/source/kind",
                "command.tool.versions[0].packages[0].source.kind",
                "hosted",
            ),
        ] {
            let mut command = example("external")["command"].clone();
            command["secret-property-token"] = json!("secret-token");
            command
                .pointer_mut("/tool/versions/0/packages/0/source")
                .unwrap()["url"] =
                json!("https://example.invalid/tool.zip?token=secret-token&sig=signed-secret");
            *command.pointer_mut(pointer).unwrap() = json!("secret-token");
            let message = parse_command(command.clone()).unwrap_err();
            for secret in [
                "secret-token",
                "signed-secret",
                "example.invalid",
                "secret-property-token",
            ] {
                assert!(!message.contains(secret), "{message}");
            }
            check_error(command, field, expected);
        }
        let mut command = example("external")["command"].clone();
        command
            .pointer_mut("/tool/versions/0/packages/0/source")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("url");
        check_error(
            command,
            "command.tool.versions[0].packages[0].source.url",
            "string",
        );
    }

    #[test]
    fn non_object_unions_point_to_the_object_and_cli_mapping_selects_its_kind() {
        for value in [
            Value::Null,
            json!([]),
            json!("secret-token"),
            json!(true),
            json!(1),
        ] {
            check_error(value.clone(), "command", "object");
            let mut command = example("external")["command"].clone();
            *command
                .pointer_mut("/tool/versions/0/packages/0/source")
                .unwrap() = value;
            check_error(
                command,
                "command.tool.versions[0].packages[0].source",
                "object",
            );
        }
        let mut command = example("hosted")["command"].clone();
        let startup = command
            .pointer_mut("/tool/versions/0/packages/0/tool/startup")
            .unwrap();
        startup["interface"] = json!("cli");
        startup["operations"] = json!([{
            "name":"echo", "description":"example", "input_schema": {"type":"object"},
            "args": [{"kind":"parameter", "name":true}]
        }]);
        check_error(
            command.clone(),
            "command.tool.versions[0].packages[0].tool.startup.operations[0].args[0].name",
            "string",
        );
        *command
            .pointer_mut("/tool/versions/0/packages/0/tool/startup/operations/0/args/0")
            .unwrap() = json!({"kind":"secret-token"});
        check_error(
            command,
            "command.tool.versions[0].packages[0].tool.startup.operations[0].args[0].kind",
            "parameter",
        );
    }

    #[test]
    fn integer_range_and_float_encoding_errors_keep_the_field_path() {
        for build in [json!(4_294_967_296_u64), json!(-1), json!(1.0), json!(1.5)] {
            let mut command = example("record")["command"].clone();
            command["record"]["windows_build"] = build;
            check_error(command, "command.record.windows_build", "integer");
        }
        for (pointer, field, replacement) in [
            (
                "/tool/versions/0/packages/0/tool/platforms/0/max_windows_build",
                "command.tool.versions[0].packages[0].tool.platforms[0].max_windows_build",
                json!(4_294_967_296_u64),
            ),
            (
                "/tool/versions/0/packages/0/tool/package/size",
                "command.tool.versions[0].packages[0].tool.package.size",
                json!(1.0),
            ),
            (
                "/tool/versions/0/packages/0/tool/health_check/timeout_secs",
                "command.tool.versions[0].packages[0].tool.health_check.timeout_secs",
                json!(1e30),
            ),
        ] {
            let mut command = example("hosted")["command"].clone();
            *command.pointer_mut(pointer).unwrap() = replacement;
            check_error(command, field, "integer");
        }
        let mut command = example("record")["command"].clone();
        command["record"]["windows_build"] = json!(u32::MAX);
        assert!(parse_command(command).is_ok());
        let mut command = example("hosted")["command"].clone();
        command
            .pointer_mut("/tool/versions/0/packages/0/tool/package")
            .unwrap()["size"] = json!(u64::MAX);
        assert!(parse_command(command).is_ok());
    }

    #[test]
    fn generated_defaults_preserve_new_tool_and_server_fact_defaults() {
        let mut command = example("hosted")["command"].clone();
        let tool = command["tool"].as_object_mut().unwrap();
        for key in ["enabled", "examples", "dependencies"] {
            tool.remove(key);
        }
        let package = command
            .pointer_mut("/tool/versions/0/packages/0")
            .unwrap()
            .as_object_mut()
            .unwrap();
        package.remove("tests");
        package.remove("file_verified");
        let ToolCenterCommand::PutTool { tool } = parse_command(command).unwrap() else {
            unreachable!()
        };
        assert!(!tool.enabled);
        assert!(tool.examples.is_empty());
        assert!(tool.dependencies.is_empty());
        assert!(!tool.versions[0].packages[0].file_verified);
        assert!(tool.versions[0].packages[0].tests.is_empty());
    }
}
