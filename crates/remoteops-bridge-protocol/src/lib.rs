//! Bridge 本机 JSON-RPC 协议；与 Relay 线协议独立演进。
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// 当前本机协议版本。
pub const VERSION: u16 = 1;
/// 单条 UTF-8 消息上限，包含换行。
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// 同时等待宿主响应的最大请求数。
pub const MAX_CONCURRENT: usize = 16;

/// JSON-RPC 2.0 消息；ID 只接受字符串或整数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// 固定为 2.0。
    pub jsonrpc: String,
    /// 请求与响应的关联标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 请求或通知的方法。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 方法参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// 成功响应。
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<Value>,
    /// 失败响应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// 稳定错误，不携带凭据和内部路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    /// JSON-RPC 错误编号。
    pub code: i32,
    /// 可读错误摘要。
    pub message: String,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl Message {
    /// 构造请求或通知。
    #[must_use]
    pub fn request(id: Option<Value>, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }
    /// 构造成功响应。
    #[must_use]
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }
    /// 构造失败响应。
    #[must_use]
    pub fn failure(id: Option<Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.unwrap_or(Value::Null)),
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
    /// 验证消息形状，不接受批处理及浮点 ID。
    #[must_use]
    pub fn valid(&self) -> bool {
        self.jsonrpc == "2.0"
            && self.id.as_ref().is_none_or(|id| {
                id.is_string()
                    || id.is_i64()
                    || id.is_u64()
                    || (id.is_null() && self.error.is_some())
            })
            && if self.method.is_some() {
                self.result.is_none() && self.error.is_none()
            } else {
                self.id.is_some()
                    && self.params.is_none()
                    && (self.result.is_some() ^ self.error.is_some())
            }
    }
}

/// 有界读取一条消息，在分配大缓冲区之前拒绝超限数据。
/// # Errors
/// 超长、缺少换行或 I/O 失败时返回错误。
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "incomplete_message",
                ))
            };
        }
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if line.len() + count > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "message_too_large",
            ));
        }
        let complete = buffer[count - 1] == b'\n';
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if complete {
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounds_and_truncated_messages() {
        assert!(read_line(&mut &b"{}\n"[..]).await.unwrap().is_some());
        assert!(read_line(&mut &b"{}"[..]).await.is_err());
        assert!(
            read_line(&mut &vec![b'x'; MAX_MESSAGE_BYTES + 1][..])
                .await
                .is_err()
        );
    }
    #[test]
    fn rejects_invalid_shapes() {
        let null_result: Message =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"a","result":null}"#).unwrap();
        assert!(null_result.valid());
        assert!(Message::success(Value::from(1), Value::Bool(true)).valid());
        assert!(!Message::request(Some(Value::from(1.2)), "status", Value::Null).valid());
    }
}
