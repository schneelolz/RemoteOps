use std::sync::Arc;

use anyhow::{Context, bail};
use async_trait::async_trait;
use remoteops_domain::{RequestId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 宿主登记的能力描述；清单和实际执行共用该登记。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostCapability {
    /// 稳定能力标识。
    pub id: String,
    /// 精确版本。
    pub version: String,
    /// 提供给调用方的业务说明。
    pub description: String,
    /// 宿主可信登记的只读属性，不接受远端自报。
    pub read_only: bool,
    /// 参数 JSON Schema 子集。
    pub input_schema: Value,
    /// 结果结构说明。
    pub output_schema: Value,
}

/// 宿主当前进程的服务身份和能力快照。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostServiceDescription {
    /// 稳定服务类型，例如 grclient 或 udserver。
    pub service_id: String,
    /// 宿主自身版本。
    pub service_version: String,
    /// 宿主本次运行标识，不能用机器名代替。
    pub instance_id: String,
    /// 业务消息版本，首版固定为 1。
    pub protocol_version: u16,
    /// 显式允许远程发现的业务能力。
    pub capabilities: Vec<HostCapability>,
}

/// 经远程会话认证后交给宿主的查询。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostQuery {
    /// 原始远程请求，禁止自动重放。
    pub request_id: RequestId,
    /// 原始已授权会话。
    pub session_id: SessionId,
    /// 被调用的精确宿主实例。
    pub instance_id: String,
    /// 能力标识。
    pub capability_id: String,
    /// 精确能力版本。
    pub version: String,
    /// 结构化参数。
    pub arguments: Value,
}

/// 宿主业务扩展，由可信本机宿主创建。
#[async_trait]
pub trait HostBusinessProvider: Send + Sync {
    /// 返回和执行入口共用的本次登记。
    fn describe(&self) -> HostServiceDescription;
    /// 执行一次只读查询；取消时释放等待，不自行重放。
    async fn query(&self, request: HostQuery) -> anyhow::Result<Value>;
}

pub(crate) fn validate_description(description: &HostServiceDescription) -> anyhow::Result<()> {
    if description.protocol_version != 1
        || description.service_id.is_empty()
        || description.instance_id.is_empty()
        || description.instance_id.len() > 128
        || description.capabilities.len() > 64
        || serde_json::to_vec(description)?.len() > 32 * 1024
    {
        bail!("invalid_host_description");
    }
    let mut keys = std::collections::BTreeSet::new();
    for capability in &description.capabilities {
        if capability.id.is_empty()
            || capability.id.len() > 128
            || capability.version.is_empty()
            || !keys.insert((&capability.id, &capability.version))
        {
            bail!("invalid_host_capability");
        }
    }
    Ok(())
}

pub(crate) async fn query(
    provider: &Arc<dyn HostBusinessProvider>,
    request: HostQuery,
) -> anyhow::Result<Value> {
    let description = provider.describe();
    validate_description(&description)?;
    if description.instance_id != request.instance_id {
        bail!("instance_changed");
    }
    let capability = description
        .capabilities
        .iter()
        .find(|capability| {
            capability.id == request.capability_id && capability.version == request.version
        })
        .context("capability_not_supported")?;
    if !capability.read_only {
        bail!("permission_denied: capability is not read-only");
    }
    if serde_json::to_vec(&request.arguments)?.len() > 240 * 1024 {
        bail!("host_arguments_too_large");
    }
    validate_value(&capability.input_schema, &request.arguments, 0)?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), provider.query(request))
        .await
        .context("host_query_timeout: result_unknown")??;
    if serde_json::to_vec(&result)?.len() > 240 * 1024 {
        bail!("host_result_too_large");
    }
    validate_value(&capability.output_schema, &result, 0)?;
    Ok(result)
}

fn validate_value(schema: &Value, value: &Value, depth: usize) -> anyhow::Result<()> {
    if depth > 16 {
        bail!("schema_depth_exceeded");
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let valid = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "null" => value.is_null(),
            _ => false,
        };
        if !valid {
            bail!("invalid_arguments_or_result");
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                if !field.as_str().is_some_and(|name| object.contains_key(name)) {
                    bail!("required_field_missing");
                }
            }
        }
        {
            let empty = serde_json::Map::new();
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            for (name, entry) in object {
                if let Some(field_schema) = properties.get(name) {
                    validate_value(field_schema, entry, depth + 1)?;
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    bail!("unexpected_field");
                }
            }
        }
    }
    if let (Some(items), Some(entries)) = (schema.get("items"), value.as_array()) {
        for entry in entries {
            validate_value(items, entry, depth + 1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Mock;
    #[async_trait]
    impl HostBusinessProvider for Mock {
        fn describe(&self) -> HostServiceDescription {
            HostServiceDescription {
                service_id: "grclient".into(),
                service_version: "1".into(),
                instance_id: "instance-a".into(),
                protocol_version: 1,
                capabilities: vec![HostCapability {
                    id: "user".into(),
                    version: "1".into(),
                    description: "只读查询".into(),
                    read_only: true,
                    input_schema: json!({"type":"object","additionalProperties":false}),
                    output_schema: json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}),
                }],
            }
        }
        async fn query(&self, _request: HostQuery) -> anyhow::Result<Value> {
            Ok(json!({"name":"模拟用户"}))
        }
    }
    fn request() -> HostQuery {
        HostQuery {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            instance_id: "instance-a".into(),
            capability_id: "user".into(),
            version: "1".into(),
            arguments: json!({}),
        }
    }
    #[tokio::test]
    async fn rejects_wrong_instance_version_and_extra_parameters() {
        let provider: Arc<dyn HostBusinessProvider> = Arc::new(Mock);
        assert_eq!(
            query(&provider, request()).await.unwrap()["name"],
            "模拟用户"
        );
        let mut wrong = request();
        wrong.instance_id = "instance-b".into();
        assert!(query(&provider, wrong).await.is_err());
        let mut wrong = request();
        wrong.version = "2".into();
        assert!(query(&provider, wrong).await.is_err());
        let mut wrong = request();
        wrong.arguments = json!({"unexpected":"value"});
        assert!(query(&provider, wrong).await.is_err());
        assert!(
            validate_value(
                &Mock.describe().capabilities[0].output_schema,
                &json!({"name":1}),
                0
            )
            .is_err()
        );
    }
}
