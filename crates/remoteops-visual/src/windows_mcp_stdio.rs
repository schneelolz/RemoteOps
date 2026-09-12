//! Windows-MCP 原生 stdio 初始化通道，供交互式 Provider 宿主使用。

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use crate::windows_provider::WindowsMcpConfig;

const PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 已完成 MCP 握手的上游身份；不代表桌面已通过观察或输入验收。
#[derive(Debug, Clone)]
pub struct WindowsMcpIdentity {
    /// 服务端协商的协议版本。
    pub protocol_version: String,
    /// 服务端实现名称。
    pub name: String,
    /// 服务端实现版本，可能与 Python 包版本不同。
    pub version: String,
}

/// 无控制台的上游 MCP 子进程；任何通信失败后均失效并回收。
pub struct WindowsMcpStdioClient {
    // 随宿主释放而终止的上游进程。
    child: Child,
    // 握手成功后才保留通道，失败后不允许复用。
    wire: Option<McpWire<ChildStdout, ChildStdin>>,
    // 经过协议验证的上游身份。
    identity: WindowsMcpIdentity,
}

impl WindowsMcpStdioClient {
    /// 验证入口文件并启动上游原生 stdio 服务，完成初始化握手。
    ///
    /// # Errors
    /// 文件校验、进程启动、协议协商或初始化超时失败时返回错误。
    pub async fn start(config: &WindowsMcpConfig) -> Result<Self, String> {
        config.verify_file()?;
        let mut command = Command::new(&config.executable);
        command
            .args(["serve", "--transport", "stdio"])
            .creation_flags(0x0800_0000)
            .kill_on_drop(true)
            .env("ANONYMIZED_TELEMETRY", "false")
            .env("WINDOWS_MCP_DISABLE_FLASH", "1")
            .env("WINDOWS_MCP_WATCHDOG", "off")
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| "Windows-MCP stdio 启动失败")?;
        let stdout = child.stdout.take().ok_or("Windows-MCP stdout 不可用")?;
        let stdin = child.stdin.take().ok_or("Windows-MCP stdin 不可用")?;
        let mut wire = McpWire::new(stdout, stdin);
        let identity = match wire.initialize(REQUEST_TIMEOUT).await {
            Ok(identity) => identity,
            Err(error) => {
                tracing::error!(%error, "Windows-MCP stdio initialization failed");
                drop(wire);
                let _ = child.kill().await;
                return Err(error);
            }
        };
        tracing::info!(pid = child.id(), protocol = %identity.protocol_version, "Windows-MCP stdio initialized");
        Ok(Self {
            child,
            wire: Some(wire),
            identity,
        })
    }

    /// 获取握手返回的上游身份。
    #[must_use]
    pub fn identity(&self) -> &WindowsMcpIdentity {
        &self.identity
    }

    /// 仅当进程存活且协议通道完整时允许复用；取消中的请求会丢弃通道。
    pub fn is_usable(&mut self) -> bool {
        self.wire.is_some() && matches!(self.child.try_wait(), Ok(None))
    }

    /// 分页发现上游工具；返回工具名称，不收集桌面内容。
    ///
    /// # Errors
    /// 通道失效、超时、响应格式错误或分页超过上限时返回错误。
    pub async fn list_tools(&mut self) -> Result<Vec<String>, String> {
        let result = self.discover_tools().await;
        if let Err(error) = &result {
            tracing::error!(%error, "Windows-MCP stdio discovery failed");
            self.stop().await;
        }
        result
    }

    /// 调用只读 Snapshot，验证当前子进程和桌面采集仍然可用。
    ///
    /// # Errors
    /// 子进程退出、请求失败或上游返回采集错误时失效并回收通道。
    pub async fn snapshot(&mut self, include_ui_tree: bool) -> Result<String, String> {
        let result = async {
            if self
                .child
                .try_wait()
                .map_err(|_| "Windows-MCP 状态查询失败")?
                .is_some()
            {
                return Err("Windows-MCP 子进程已退出".into());
            }
            // 请求被外层取消时丢弃整条通道，避免下次读到旧请求的响应。
            let mut wire = self.wire.take().ok_or("Windows-MCP stdio 通道已失效")?;
            let result = wire
                .request(
                    "tools/call",
                    json!({
                        "name":"Snapshot", "arguments":{
                            "use_vision":false, "use_ui_tree":include_ui_tree,
                            "use_annotation":false, "use_dom":false
                        }
                    }),
                    REQUEST_TIMEOUT,
                )
                .await?;
            let text = snapshot_text(&result, include_ui_tree)?;
            self.wire = Some(wire);
            Ok(text)
        }
        .await;
        if result.is_err() {
            self.stop().await;
        }
        result
    }

    async fn discover_tools(&mut self) -> Result<Vec<String>, String> {
        let wire = self.wire.as_mut().ok_or("Windows-MCP stdio 通道已失效")?;
        let mut names = Vec::new();
        let mut params = json!({});
        for _ in 0..16 {
            let result = wire.request("tools/list", params, REQUEST_TIMEOUT).await?;
            for tool in result["tools"]
                .as_array()
                .ok_or("Windows-MCP tools/list 格式错误")?
            {
                names.push(
                    tool["name"]
                        .as_str()
                        .ok_or("Windows-MCP 工具缺少名称")?
                        .to_owned(),
                );
            }
            match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(names),
                Some(Value::String(cursor)) if !cursor.is_empty() => {
                    params = json!({"cursor": cursor});
                }
                _ => return Err("Windows-MCP 分页游标无效".into()),
            }
        }
        Err("Windows-MCP 工具分页超出上限".into())
    }

    /// 关闭输入流，给子进程退出时间，随后强制回收并记录结果。
    pub async fn stop(&mut self) {
        self.wire.take();
        if let Ok(Ok(status)) = timeout(Duration::from_secs(2), self.child.wait()).await {
            tracing::info!(code = status.code(), "Windows-MCP stdio stopped");
        } else {
            let _ = self.child.kill().await;
            tracing::warn!("Windows-MCP stdio forced stop");
        }
    }
}

/// 锁定上游使用文本内容块；文本错误也必须拒绝，不能仅检查 JSON-RPC 成功。
fn snapshot_text(result: &Value, include_ui_tree: bool) -> Result<String, String> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err("Windows-MCP Snapshot 返回工具错误".into());
    }
    let content = result["content"]
        .as_array()
        .ok_or("Windows-MCP Snapshot 缺少内容块")?;
    let mut text = String::new();
    for block in content {
        if block["type"] == "text" {
            let value = block["text"].as_str().ok_or("Windows-MCP 文本内容块无效")?;
            if value.trim_start().starts_with("Error capturing") {
                return Err("Windows-MCP Snapshot 桌面采集失败".into());
            }
            text.push_str(value);
            text.push('\n');
        }
    }
    if !text.contains("Focused Window:")
        || !text.contains("Opened Windows:")
        || (include_ui_tree && !text.contains("UI Tree:"))
    {
        return Err("Windows-MCP Snapshot 内容格式不兼容".into());
    }
    Ok(text)
}

/// 可用内存双工流验证的单请求 JSON-RPC 通道。
struct McpWire<R, W> {
    // 保留分帧读取缓冲，避免丢失相邻消息。
    reader: BufReader<R>,
    // 仅写入 UTF-8 JSON-RPC 消息。
    writer: W,
    // 单调递增的本地请求编号。
    next_id: u64,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> McpWire<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        }
    }

    async fn send(&mut self, value: Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| "Windows-MCP 请求编码失败")?;
        bytes.push(b'\n');
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|_| "Windows-MCP 写入失败")?;
        self.writer
            .flush()
            .await
            .map_err(|_| "Windows-MCP 刷新失败".into())
    }

    async fn read(&mut self) -> Result<Value, String> {
        let mut bytes = Vec::new();
        loop {
            let chunk = self
                .reader
                .fill_buf()
                .await
                .map_err(|_| "Windows-MCP 读取失败")?;
            if chunk.is_empty() {
                return Err("Windows-MCP stdout 已关闭".into());
            }
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(chunk.len(), |index| index + 1);
            if bytes.len() + count > MAX_FRAME_BYTES {
                return Err("Windows-MCP 响应帧超出上限".into());
            }
            bytes.extend_from_slice(&chunk[..count]);
            self.reader.consume(count);
            if newline.is_some() {
                return serde_json::from_slice(&bytes)
                    .map_err(|_| "Windows-MCP 返回无效 JSON".into());
            }
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, String> {
        timeout(limit, self.exchange(method, params))
            .await
            .map_err(|_| "Windows-MCP 请求超时".to_owned())?
    }

    async fn exchange(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("Windows-MCP 请求编号耗尽")?;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        for _ in 0..128 {
            let message = self.read().await?;
            if message["jsonrpc"] != "2.0" {
                return Err("Windows-MCP JSON-RPC 版本错误".into());
            }
            if let Some(method) = message.get("method") {
                if let Some(server_id) = message.get("id") {
                    let reply = if method == "ping" {
                        json!({"jsonrpc":"2.0","id":server_id,"result":{}})
                    } else {
                        json!({"jsonrpc":"2.0","id":server_id,"error":{"code":-32601,"message":"Unsupported method"}})
                    };
                    self.send(reply).await?;
                }
                continue;
            }
            if message["id"] != id {
                return Err("Windows-MCP 响应编号不匹配".into());
            }
            if message.get("error").is_some() {
                return Err("Windows-MCP 返回协议错误".into());
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| "Windows-MCP 响应缺少 result".into());
        }
        Err("Windows-MCP 消息数量超出上限".into())
    }

    async fn initialize(&mut self, limit: Duration) -> Result<WindowsMcpIdentity, String> {
        let result = self.request("initialize", json!({
            "protocolVersion":PROTOCOL_VERSION,"capabilities":{},
            "clientInfo":{"name":"remoteops-visual-provider","version":env!("CARGO_PKG_VERSION")}
        }), limit).await?;
        if result["protocolVersion"] != PROTOCOL_VERSION {
            return Err("Windows-MCP 协议版本不兼容".into());
        }
        if !result["capabilities"]["tools"].is_object() {
            return Err("Windows-MCP 未声明工具能力".into());
        }
        let identity = WindowsMcpIdentity {
            protocol_version: PROTOCOL_VERSION.into(),
            name: result["serverInfo"]["name"]
                .as_str()
                .ok_or("Windows-MCP 缺少服务端名称")?
                .into(),
            version: result["serverInfo"]["version"]
                .as_str()
                .ok_or("Windows-MCP 缺少服务端版本")?
                .into(),
        };
        timeout(
            limit,
            self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        )
        .await
        .map_err(|_| "Windows-MCP initialized 通知超时".to_owned())??;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    #[test]
    fn snapshot_rejects_tool_and_text_errors_and_missing_sections() {
        for value in [
            json!({"isError":true,"content":[]}),
            json!({"content":[{"type":"text","text":"Error capturing desktop state: failed"}]}),
            json!({"content":[{"type":"text","text":"Focused Window:"}]}),
        ] {
            assert!(snapshot_text(&value, true).is_err());
        }
        let result = json!({"content":[{"type":"text","text":"Focused Window:\nExplorer\nOpened Windows:\nExplorer\nUI Tree:\nButton"}]});
        assert!(snapshot_text(&result, true).unwrap().contains("Button"));
    }

    #[tokio::test]
    async fn initializes_and_discovers_tools_over_json_rpc() {
        let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let (server_read, mut server_write) = tokio::io::split(server_stream);
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            server_write.write_all(br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"windows-mcp","version":"4.0.3"}}}
"#).await.unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            server_write.write_all(br#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"Snapshot"},{"name":"Screenshot"}]}}
"#).await.unwrap();
        });
        let mut wire = McpWire::new(client_read, client_write);
        let identity = wire.initialize(Duration::from_secs(2)).await.unwrap();
        assert_eq!(identity.name, "windows-mcp");
        let tools = wire
            .request("tools/list", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        let names: Vec<_> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Snapshot", "Screenshot"]);
    }

    #[tokio::test]
    async fn rejects_oversized_frames() {
        let (client_stream, server_stream) = tokio::io::duplex(MAX_FRAME_BYTES + 16);
        let (client_read, _client_write) = tokio::io::split(client_stream);
        let (_server_read, mut server_write) = tokio::io::split(server_stream);
        let oversized = vec![b'a'; MAX_FRAME_BYTES + 1];
        server_write.write_all(&oversized).await.unwrap();
        let mut wire = McpWire::new(client_read, tokio::io::sink());
        assert_eq!(wire.read().await.unwrap_err(), "Windows-MCP 响应帧超出上限");
    }
}
