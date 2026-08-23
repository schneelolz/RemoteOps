//! `RemoteOps` 壳子共用的 `OpenAI` 兼容 Agent 客户端。
#![allow(clippy::missing_errors_doc)]

use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;

/// 支持的 `OpenAI` 兼容接口协议。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProtocol {
    /// 先尝试 Responses API，不兼容时回退 Chat Completions。
    #[default]
    Auto,
    /// `OpenAI` Responses API。
    Responses,
    /// `OpenAI` Chat Completions API。
    ChatCompletions,
}

impl AiProtocol {
    /// 解析命令行或配置文件中的协议名称。
    pub fn parse(value: &str) -> Result<Self, AiError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "responses" => Ok(Self::Responses),
            "chat" | "chat_completions" | "chat-completions" => Ok(Self::ChatCompletions),
            _ => Err(AiError::Configuration(
                "AI 协议必须是 auto、responses 或 chat".to_owned(),
            )),
        }
    }

    fn from_codex_wire_api(value: Option<&str>) -> Self {
        value
            .map_or(Ok(Self::Auto), Self::parse)
            .unwrap_or(Self::Auto)
    }
}

/// AI 客户端运行配置。
#[derive(Clone)]
pub struct AiClientConfig {
    /// API 基础地址，通常以 `/v1` 结尾。
    pub base_url: String,
    /// 模型名称。
    pub model: String,
    /// Bearer Token。
    pub bearer_token: String,
    /// 调用协议。
    pub protocol: AiProtocol,
}

impl AiClientConfig {
    /// 校验并规范化配置。
    pub fn normalized(mut self) -> Result<Self, AiError> {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        self.model = self.model.trim().to_owned();
        self.bearer_token = self.bearer_token.trim().to_owned();
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err(AiError::Configuration(
                "AI 服务地址必须以 http:// 或 https:// 开头".to_owned(),
            ));
        }
        if self.model.is_empty() {
            return Err(AiError::Configuration("AI 模型名称不能为空".to_owned()));
        }
        if self.bearer_token.is_empty() {
            return Err(AiError::Configuration(
                "AI Bearer Token 不能为空".to_owned(),
            ));
        }
        Ok(self)
    }
}

impl std::fmt::Debug for AiClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiClientConfig")
            .field(
                "base_url",
                &redact_secret(&self.base_url, &self.bearer_token),
            )
            .field("model", &self.model)
            .field("bearer_token", &"[已隐藏]")
            .field("protocol", &self.protocol)
            .finish()
    }
}

/// 一轮已经完成的对话。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationTurn {
    /// 用户输入。
    pub user: String,
    /// AI 最终回答。
    pub assistant: String,
}

/// 提供给模型的函数工具定义。
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    /// 工具名称。
    pub name: String,
    /// 工具用途。
    pub description: String,
    /// JSON Schema 参数定义。
    pub parameters: Value,
}

/// 模型请求执行的一次工具调用。
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// 协议返回的调用标识。
    pub id: String,
    /// 工具名称。
    pub name: String,
    /// 已解析的 JSON 参数。
    pub arguments: Value,
}

/// 一次 Agent 对话请求。
pub struct AgentRequest<'a> {
    /// 系统级行为约束。
    pub instructions: &'a str,
    /// 已完成的短期对话历史。
    pub history: &'a [ConversationTurn],
    /// 当前用户目标。
    pub prompt: &'a str,
    /// 本轮允许使用的工具。
    pub tools: &'a [ToolDefinition],
    /// 最大工具往返轮数；零值使用默认值。
    pub max_tool_rounds: usize,
}

/// 应用壳负责实现的工具执行入口。
#[async_trait]
pub trait ToolExecutor: Send {
    /// 执行一次模型请求的工具调用，并返回可发送给模型的文字结果。
    async fn execute(&mut self, call: &ToolCall) -> Result<String, String>;
}

/// AI 客户端错误。
#[derive(Debug, Error)]
pub enum AiError {
    /// 本地配置无效。
    #[error("AI 配置无效：{0}")]
    Configuration(String),
    /// 接口调用或响应解析失败。
    #[error("AI 调用失败：{0}")]
    Protocol(String),
    /// 工具执行失败。
    #[error("AI 工具执行失败：{0}")]
    Tool(String),
}

#[derive(Debug)]
struct ProtocolError {
    message: String,
    fallback_allowed: bool,
}

impl ProtocolError {
    fn new(message: impl Into<String>, fallback_allowed: bool) -> Self {
        Self {
            message: message.into(),
            fallback_allowed,
        }
    }
}

/// 支持函数工具循环的 `OpenAI` 兼容客户端。
pub struct AiClient {
    http: Client,
    config: AiClientConfig,
}

impl AiClient {
    /// 创建客户端并校验配置。
    pub fn new(config: AiClientConfig) -> Result<Self, AiError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_mins(2))
                .build()
                .map_err(|error| AiError::Configuration(error.to_string()))?,
            config: config.normalized()?,
        })
    }

    /// 返回不含 Token 的模型和协议摘要。
    #[must_use]
    pub fn description(&self) -> String {
        format!("{} · {:?}", self.config.model, self.config.protocol)
    }

    /// 运行一轮可以多次调用工具的 Agent 对话。
    pub async fn complete_with_tools<E: ToolExecutor>(
        &self,
        request: AgentRequest<'_>,
        executor: &mut E,
    ) -> Result<String, AiError> {
        let max_rounds = if request.max_tool_rounds == 0 {
            DEFAULT_MAX_TOOL_ROUNDS
        } else {
            request.max_tool_rounds.clamp(1, 16)
        };
        let result = match self.config.protocol {
            AiProtocol::Responses => {
                self.complete_responses(&request, executor, max_rounds)
                    .await
            }
            AiProtocol::ChatCompletions => self.complete_chat(&request, executor, max_rounds).await,
            AiProtocol::Auto => {
                match self
                    .complete_responses(&request, executor, max_rounds)
                    .await
                {
                    Ok(answer) => Ok(answer),
                    Err(error) if error.fallback_allowed => {
                        self.complete_chat(&request, executor, max_rounds).await
                    }
                    Err(error) => Err(error),
                }
            }
        };
        result.map_err(|error| AiError::Protocol(self.redact_error(&error.message)))
    }

    async fn complete_chat<E: ToolExecutor>(
        &self,
        request: &AgentRequest<'_>,
        executor: &mut E,
        max_rounds: usize,
    ) -> Result<String, ProtocolError> {
        let mut messages = chat_messages(request);
        let tools = chat_tools(request.tools);
        for _ in 0..max_rounds {
            let response = self
                .http
                .post(api_endpoint(&self.config.base_url, "chat/completions"))
                .bearer_auth(&self.config.bearer_token)
                .json(&serde_json::json!({
                    "model": self.config.model,
                    "messages": messages,
                    "tools": tools,
                    "tool_choice": "auto"
                }))
                .send()
                .await
                .map_err(|error| ProtocolError::new(error.to_string(), false))?;
            let status = response.status();
            if !status.is_success() {
                return Err(ProtocolError::new(
                    format!("Chat Completions 返回 HTTP {status}"),
                    false,
                ));
            }
            let response = response
                .json::<ChatResponse>()
                .await
                .map_err(|error| ProtocolError::new(format!("响应解析失败：{error}"), false))?;
            let message = response
                .choices
                .into_iter()
                .next()
                .map(|choice| choice.message)
                .ok_or_else(|| ProtocolError::new("接口没有返回候选回答", false))?;
            let calls = message.tool_calls.clone().unwrap_or_default();
            if calls.is_empty() {
                return message
                    .content
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| ProtocolError::new("接口没有返回文字回答", false));
            }
            messages.push(message);
            for call in calls {
                let arguments = serde_json::from_str(&call.function.arguments)
                    .map_err(|error| ProtocolError::new(format!("工具参数无效：{error}"), false))?;
                let result = executor
                    .execute(&ToolCall {
                        id: call.id.clone(),
                        name: call.function.name,
                        arguments,
                    })
                    .await
                    .map_err(|error| ProtocolError::new(error, false))?;
                messages.push(ChatMessage {
                    role: "tool".to_owned(),
                    content: Some(result),
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                });
            }
        }
        Err(ProtocolError::new("工具调用超过最大轮数", false))
    }

    async fn complete_responses<E: ToolExecutor>(
        &self,
        request: &AgentRequest<'_>,
        executor: &mut E,
        max_rounds: usize,
    ) -> Result<String, ProtocolError> {
        let mut input = responses_conversation(request);
        let tools = responses_tools(request.tools);
        let mut tool_was_executed = false;
        for _ in 0..max_rounds {
            let response = self
                .http
                .post(api_endpoint(&self.config.base_url, "responses"))
                .bearer_auth(&self.config.bearer_token)
                .json(&serde_json::json!({
                    "model": self.config.model,
                    "instructions": request.instructions,
                    "input": input,
                    "tools": tools,
                    "tool_choice": "auto"
                }))
                .send()
                .await
                .map_err(|error| ProtocolError::new(error.to_string(), false))?;
            let status = response.status();
            if !status.is_success() {
                return Err(ProtocolError::new(
                    format!("Responses API 返回 HTTP {status}"),
                    !tool_was_executed && protocol_fallback_status(status),
                ));
            }
            let response = response
                .json::<ResponsesResponse>()
                .await
                .map_err(|error| ProtocolError::new(format!("响应解析失败：{error}"), false))?;
            let calls = responses_tool_calls(&response.output)?;
            if calls.is_empty() {
                let text = responses_text(&response.output);
                if text.trim().is_empty() {
                    return Err(ProtocolError::new(
                        "接口没有返回文字回答",
                        !tool_was_executed,
                    ));
                }
                return Ok(text);
            }
            input.extend(response.output);
            for call in calls {
                tool_was_executed = true;
                let result = executor
                    .execute(&call)
                    .await
                    .map_err(|error| ProtocolError::new(error, false))?;
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call.id,
                    "output": result
                }));
            }
        }
        Err(ProtocolError::new("工具调用超过最大轮数", false))
    }

    fn redact_error(&self, message: &str) -> String {
        redact_secret(message, &self.config.bearer_token)
    }
}

/// 从 Codex 当前 Provider 配置中读取模型、地址、协议和 Token。
pub fn import_from_codex(explicit_path: Option<&Path>) -> Result<AiClientConfig, AiError> {
    let path = explicit_path
        .map(Path::to_path_buf)
        .or_else(|| {
            codex_config_candidates()
                .into_iter()
                .find(|candidate| candidate.is_file())
        })
        .ok_or_else(|| AiError::Configuration("未找到 Codex 配置文件".to_owned()))?;
    let text = std::fs::read_to_string(&path).map_err(|error| {
        AiError::Configuration(format!("无法读取 Codex 配置 {}：{error}", path.display()))
    })?;
    let config = toml::from_str::<CodexConfig>(&text)
        .map_err(|_| AiError::Configuration(format!("Codex 配置格式无效：{}", path.display())))?;
    let provider_name = config
        .model_provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AiError::Configuration("Codex 配置未指定 model_provider".to_owned()))?;
    let provider = config.model_providers.get(provider_name).ok_or_else(|| {
        AiError::Configuration(format!(
            "Codex 配置中不存在 model_providers.{provider_name}"
        ))
    })?;
    AiClientConfig {
        base_url: provider.base_url.clone().unwrap_or_default(),
        model: config.model.unwrap_or_default(),
        bearer_token: provider
            .experimental_bearer_token
            .clone()
            .unwrap_or_default(),
        protocol: AiProtocol::from_codex_wire_api(provider.wire_api.as_deref()),
    }
    .normalized()
}

fn codex_config_candidates() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(r"C:\Codex\.codex\config.toml")];
    if let Some(codex_home) = env::var_os("CODEX_HOME") {
        paths.push(PathBuf::from(codex_home).join("config.toml"));
    }
    if let Some(user_profile) = env::var_os("USERPROFILE") {
        paths.push(
            PathBuf::from(user_profile)
                .join(".codex")
                .join("config.toml"),
        );
    }
    paths.dedup();
    paths
}

fn api_endpoint(base_url: &str, operation: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), operation)
}

fn protocol_fallback_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 404 | 405 | 422)
}

fn redact_secret(value: &str, secret: &str) -> String {
    if secret.is_empty() {
        value.to_owned()
    } else {
        value.replace(secret, "[已隐藏]")
    }
}

fn chat_messages(request: &AgentRequest<'_>) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage {
        role: "system".to_owned(),
        content: Some(request.instructions.to_owned()),
        tool_calls: None,
        tool_call_id: None,
    }];
    for turn in request.history {
        messages.push(ChatMessage::plain("user", &turn.user));
        messages.push(ChatMessage::plain("assistant", &turn.assistant));
    }
    messages.push(ChatMessage::plain("user", request.prompt));
    messages
}

fn responses_conversation(request: &AgentRequest<'_>) -> Vec<Value> {
    let mut input = Vec::new();
    for turn in request.history {
        input.push(serde_json::json!({"role": "user", "content": turn.user}));
        input.push(serde_json::json!({"role": "assistant", "content": turn.assistant}));
    }
    input.push(serde_json::json!({"role": "user", "content": request.prompt}));
    input
}

fn chat_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters
                }
            })
        })
        .collect()
}

fn responses_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": true
            })
        })
        .collect()
}

fn responses_tool_calls(output: &[Value]) -> Result<Vec<ToolCall>, ProtocolError> {
    output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(|item| {
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            Ok(ToolCall {
                id: item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProtocolError::new("Responses 工具调用缺少 call_id", false))?
                    .to_owned(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProtocolError::new("Responses 工具调用缺少名称", false))?
                    .to_owned(),
                arguments: serde_json::from_str(arguments)
                    .map_err(|error| ProtocolError::new(format!("工具参数无效：{error}"), false))?,
            })
        })
        .collect()
}

fn responses_text(output: &[Value]) -> String {
    output
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl ChatMessage {
    fn plain(role: &str, content: &str) -> Self {
        Self {
            role: role.to_owned(),
            content: Some(content.to_owned()),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChatToolCall {
    id: String,
    function: ChatFunctionCall,
    #[serde(rename = "type", default = "default_tool_type")]
    call_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

fn default_tool_type() -> String {
    "function".to_owned()
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<Value>,
}

#[derive(Deserialize)]
struct CodexConfig {
    model: Option<String>,
    model_provider: Option<String>,
    #[serde(default)]
    model_providers: BTreeMap<String, CodexProvider>,
}

#[derive(Deserialize)]
struct CodexProvider {
    base_url: Option<String>,
    wire_api: Option<String>,
    experimental_bearer_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_token() {
        let config = AiClientConfig {
            base_url: "https://example.test/v1?token=secret-value".to_owned(),
            model: "test-model".to_owned(),
            bearer_token: "secret-value".to_owned(),
            protocol: AiProtocol::Responses,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-value"));
        assert!(debug.contains("[已隐藏]"));
    }

    #[test]
    fn responses_output_extracts_tool_calls_and_text() {
        let output = vec![
            serde_json::json!({
                "type": "function_call",
                "call_id": "call-1",
                "name": "read_serial_buffer",
                "arguments": "{\"max_chars\":1000}"
            }),
            serde_json::json!({
                "type": "message",
                "content": [{"type": "output_text", "text": "设备正常"}]
            }),
        ];
        let calls = responses_tool_calls(&output).expect("工具调用应可解析");
        assert_eq!(calls[0].name, "read_serial_buffer");
        assert_eq!(calls[0].arguments["max_chars"], 1000);
        assert_eq!(responses_text(&output), "设备正常");
    }

    #[test]
    fn codex_import_does_not_echo_malformed_secret_source() {
        let path =
            std::env::temp_dir().join(format!("remoteops-ai-invalid-{}.toml", std::process::id()));
        std::fs::write(&path, "experimental_bearer_token = \"secret-never-echo\n")
            .expect("测试配置应可写入");
        let error = import_from_codex(Some(&path)).expect_err("损坏配置必须失败");
        let _ = std::fs::remove_file(path);
        assert!(!error.to_string().contains("secret-never-echo"));
    }
}
