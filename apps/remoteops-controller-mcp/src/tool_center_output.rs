//! MCP 专属返回契约；共享领域类型不依赖 JSON Schema。

use remoteops_tool_center::{ToolCenterError, ToolCenterResponse};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ToolCenterOutput {
    request_id: String,
    revision: u64,
    #[schemars(required, schema_with = "nullable_error_schema")]
    error: Option<ToolCenterOutputError>,
    #[schemars(schema_with = "any_json_schema")]
    data: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ToolCenterOutputError {
    code: String,
    message: String,
}

fn nullable_error_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<Option<ToolCenterOutputError>>()
}

// 使用对象形式的任意 JSON Schema，兼容拒绝布尔 Schema 的严格 MCP SDK。
pub(super) fn any_json_schema(_: &mut SchemaGenerator) -> Schema {
    Schema::default()
}

impl From<ToolCenterResponse> for ToolCenterOutput {
    fn from(response: ToolCenterResponse) -> Self {
        Self {
            request_id: response.request_id,
            revision: response.revision,
            error: response
                .error
                .map(|ToolCenterError { code, message }| ToolCenterOutputError { code, message }),
            data: response.data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::tool::IntoCallToolResult;
    use serde_json::json;

    #[test]
    fn schema_is_object_with_nullable_error_and_arbitrary_json_data() {
        let schema = serde_json::to_value(schemars::schema_for!(ToolCenterOutput)).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["data"], json!({}));
        for field in ["request_id", "revision", "error", "data"] {
            assert!(schema["properties"].get(field).is_some());
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(field))
            );
        }
        assert!(
            schema["properties"]["error"]["anyOf"]
                .as_array()
                .unwrap()
                .iter()
                .any(|schema| schema["type"] == "null")
        );
    }

    #[test]
    fn structured_and_text_results_preserve_domain_contract_for_all_payloads() {
        for data in [
            Value::Null,
            json!({"enabled": false, "tools": []}),
            json!({"file_verified": true, "sha256": "a".repeat(64)}),
            json!([1, "data"]),
            json!(false),
            json!(42),
            json!("text"),
        ] {
            for error in [
                None,
                Some(ToolCenterError {
                    code: "revision_conflict".into(),
                    message: "修订号已变化".into(),
                }),
            ] {
                let response = ToolCenterResponse {
                    request_id: "fixture-request".into(),
                    revision: 7,
                    error,
                    data: data.clone(),
                };
                let expected = serde_json::to_value(&response).unwrap();
                let output = rmcp::Json(ToolCenterOutput::from(response))
                    .into_call_tool_result()
                    .unwrap();
                let rmcp::model::CallToolResponse::Complete(output) = output else {
                    panic!("固定工具返回不应请求额外输入");
                };
                assert_eq!(output.structured_content.as_ref(), Some(&expected));
                let text = output
                    .content
                    .iter()
                    .find_map(|content| content.as_text())
                    .unwrap();
                assert_eq!(serde_json::from_str::<Value>(&text.text).unwrap(), expected);
            }
        }
    }
}
