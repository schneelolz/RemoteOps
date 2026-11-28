use std::{
    collections::VecDeque,
    io::Write as _,
    path::Path,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, SystemTime},
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{io::AsyncReadExt as _, process::ChildStderr, task::JoinHandle};

const MAX_STDERR: usize = 16 * 1024;
const MAX_RECORD: usize = 64 * 1024;

/// 仅由适配器生成的启动上下文，不含登记参数和环境变量值。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchContext {
    /// 实际执行接口。
    pub interface: String,
    /// STDIO MCP 最终适配模式；CLI 为 null。
    pub browser_adapter: Option<String>,
    /// 显式配置、旧默认规则或独立 CLI。
    pub adapter_source: String,
    /// 适配器追加参数，路径使用固定占位符。
    pub injected_args: Vec<String>,
    /// 适配器注入的环境变量名称，不含值。
    pub injected_env_names: Vec<String>,
}

/// 可跨 Agent、Relay 和 MCP 返回的启动诊断，不包含原始报文正文。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartupDiagnostics {
    /// 启动前固定的安全上下文；旧记录可能缺失。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_context: Option<LaunchContext>,
    /// 创建进程、握手、发现或浏览器健康检查阶段。
    pub stage: String,
    /// 稳定失败原因分类。
    pub reason: String,
    /// 能够取得的真实退出码；超时或强制终止时可能为空。
    pub exit_code: Option<i32>,
    /// 已过滤路径、URL 和凭据行的 stderr 尾部，最多 16 KiB。
    pub stderr_tail: String,
    /// 最后发送报文的结构摘要，不含参数值。
    pub last_sent: Option<Value>,
    /// 最后接收报文的结构摘要，不含结果正文或错误消息。
    pub last_received: Option<Value>,
    /// 保存成功时的诊断文件名，不包含本地绝对路径。
    pub record_id: Option<String>,
}

pub(crate) struct Trace {
    launch_context: Option<LaunchContext>,
    stage: &'static str,
    reason: &'static str,
    exit_code: Option<i32>,
    stderr: VecDeque<u8>,
    truncated: bool,
    sent: Option<Value>,
    received: Option<Value>,
    active: bool,
    readers: Vec<JoinHandle<()>>,
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            launch_context: None,
            stage: "spawn",
            reason: "initialization_failed",
            exit_code: None,
            stderr: VecDeque::new(),
            truncated: false,
            sent: None,
            received: None,
            active: true,
            readers: Vec::new(),
        }
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

impl Trace {
    pub(crate) fn launch(&mut self, context: LaunchContext) {
        self.launch_context = Some(context);
    }

    pub(crate) fn stage(&mut self, stage: &'static str) {
        self.stage = stage;
        self.reason = "initialization_failed";
    }

    pub(crate) fn reason(&mut self, reason: &'static str) {
        self.reason = reason;
    }

    pub(crate) fn exit(&mut self, code: Option<i32>) {
        self.exit_code = code;
    }

    pub(crate) fn expects_exit(&self) -> bool {
        self.reason == "eof"
    }

    pub(crate) fn sent(&mut self, message: &Value) {
        if self.active {
            self.sent = Some(summary(message));
        }
    }

    pub(crate) fn received(&mut self, message: &Value) {
        if self.active {
            self.received = Some(summary(message));
        }
    }

    pub(crate) fn invalid_received(&mut self, bytes: usize) {
        if self.active {
            self.received = Some(json!({"invalid_json":true,"bytes":bytes}));
        }
    }

    pub(crate) fn finish(&mut self) {
        self.active = false;
        self.stderr.clear();
        self.sent = None;
        self.received = None;
    }

    pub(crate) fn snapshot(&self, paths: &[&Path]) -> StartupDiagnostics {
        let bytes: Vec<_> = self.stderr.iter().copied().collect();
        // 尾部截断可能切掉凭据字段名；丢弃首个不完整行再脱敏。
        let bytes = if self.truncated {
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(&[][..], |end| &bytes[end + 1..])
        } else {
            &bytes
        };
        StartupDiagnostics {
            launch_context: self.launch_context.clone(),
            stage: self.stage.into(),
            reason: self.reason.into(),
            exit_code: self.exit_code,
            stderr_tail: redact(&String::from_utf8_lossy(bytes), paths),
            last_sent: self.sent.clone(),
            last_received: self.received.clone(),
            record_id: None,
        }
    }
}

pub(crate) fn read_stderr(trace: &Arc<Mutex<Trace>>, mut pipe: ChildStderr) {
    // 弱引用避免读取任务与保存其句柄的实例形成循环。
    let weak = Arc::downgrade(trace);
    let task = tokio::spawn(async move {
        let mut buffer = [0_u8; 4096];
        while let Ok(count) = pipe.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            let Some(trace) = weak.upgrade() else {
                break;
            };
            let mut trace = trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if trace.active {
                trace.stderr.extend(&buffer[..count]);
                while trace.stderr.len() > MAX_STDERR {
                    trace.stderr.pop_front();
                    trace.truncated = true;
                }
            }
        }
    });
    let mut trace = trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    trace.readers.retain(|task| !task.is_finished());
    trace.readers.push(task);
}

fn summary(message: &Value) -> Value {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(|method| match method {
            "initialize" | "notifications/initialized" | "tools/list" | "tools/call" => method,
            _ => "other",
        });
    json!({
        "method":method,
        "id":message.get("id").and_then(Value::as_u64),
        "has_result":message.get("result").is_some(),
        "error_code":message.get("error").and_then(|error|error.get("code")).and_then(Value::as_i64),
    })
}

fn redact(text: &str, paths: &[&Path]) -> String {
    static URL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b[a-z][a-z0-9+.-]*://[^\s\x22<>]+").expect("URL pattern")
    });
    static PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:[a-z]:[\\/]|\\\\|/)[^\s\x22<>]+").expect("path pattern")
    });
    static OPAQUE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=-]{32,}").expect("opaque credential pattern"));
    let mut text = text.to_owned();
    for path in paths {
        let path = path.to_string_lossy();
        if !path.is_empty() {
            text = text.replace(path.as_ref(), "[path]");
        }
    }
    let text: String = text
        .chars()
        .filter(|value| !value.is_control() || matches!(value, '\n' | '\t'))
        .collect();
    let text = text
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "token",
                "secret",
                "password",
                "authorization",
                "credential",
                "cookie",
                "api_key",
                "apikey",
                "bearer",
                "passphrase",
                "private key",
                "private_key",
                "access_key",
                "pwd",
                "密码",
                "令牌",
                "密钥",
            ]
            .iter()
            .any(|word| lower.contains(word))
            {
                "[credential line redacted]".into()
            } else {
                OPAQUE
                    .replace_all(
                        &PATH.replace_all(&URL.replace_all(line, "[url]"), "[path]"),
                        "[opaque]",
                    )
                    .into_owned()
            }
        })
        .collect::<Vec<String>>()
        .join("\n");
    truncate_tail(&text, MAX_STDERR).to_owned()
}

fn truncate_tail(text: &str, limit: usize) -> &str {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub(crate) fn save(root: &Path, diagnostics: &mut StartupDiagnostics) {
    static SAVE_GATE: Mutex<()> = Mutex::new(());
    let _gate = SAVE_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = root.join("diagnostics");
    if std::fs::create_dir_all(&directory).is_err()
        || !directory
            .symlink_metadata()
            .is_ok_and(|metadata| !is_link(&metadata))
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        if !metadata.is_file()
            || is_link(&metadata)
            || path.extension().is_none_or(|value| value != "json")
            || path
                .file_stem()
                .and_then(|value| value.to_str())
                .is_none_or(|value| uuid::Uuid::parse_str(value).is_err())
        {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default()
            > Duration::from_hours(168)
        {
            let _ = std::fs::remove_file(&path);
        } else {
            records.push((modified, path));
        }
    }
    records.sort_by_key(|(modified, _)| *modified);
    for (_, path) in records.iter().take(records.len().saturating_sub(99)) {
        let _ = std::fs::remove_file(path);
    }
    let name = format!("{}.json", uuid::Uuid::new_v4());
    diagnostics.record_id = Some(name.clone());
    let Ok(bytes) = serde_json::to_vec(diagnostics) else {
        diagnostics.record_id = None;
        return;
    };
    if bytes.len() > MAX_RECORD {
        diagnostics.record_id = None;
        return;
    }
    let saved = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(&name))
        .and_then(|mut file| file.write_all(&bytes));
    if saved.is_err() {
        diagnostics.record_id = None;
    }
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_diagnostics_without_launch_context_remain_readable() {
        let mut value = serde_json::to_value(Trace::default().snapshot(&[])).unwrap();
        value.as_object_mut().unwrap().remove("launch_context");
        let diagnostic: StartupDiagnostics = serde_json::from_value(value).unwrap();
        assert!(diagnostic.launch_context.is_none());
    }

    #[test]
    fn redaction_and_summaries_exclude_sensitive_contents() {
        let redacted = redact(
            "error C:\\private\\tool.exe\nToken=hidden\nsee https://test.invalid/a?signature=hidden\n/home/user/file",
            &[],
        );
        assert!(!redacted.contains("private"));
        assert!(!redacted.contains("hidden"));
        assert!(!redacted.contains("/home/user"));
        let message = summary(
            &json!({"id":1,"method":"tools/call","params":{"password":"hidden"},"error":{"code":-1,"message":"hidden"}}),
        );
        assert!(!message.to_string().contains("hidden"));
        assert_eq!(message["error_code"], -1);
    }

    #[test]
    fn records_are_bounded_and_retained_separately() {
        let root = tempfile::tempdir().unwrap();
        let mut diagnostic = Trace::default().snapshot(&[]);
        for _ in 0..105 {
            save(root.path(), &mut diagnostic);
        }
        assert_eq!(
            std::fs::read_dir(root.path().join("diagnostics"))
                .unwrap()
                .count(),
            100
        );
        assert!(diagnostic.record_id.is_some());
        assert!(serde_json::to_vec(&diagnostic).unwrap().len() <= MAX_RECORD);
        let expired = root
            .path()
            .join("diagnostics")
            .join(diagnostic.record_id.clone().unwrap());
        std::fs::File::options()
            .write(true)
            .open(&expired)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_hours(192))
            .unwrap();
        save(root.path(), &mut diagnostic);
        assert!(!expired.exists());
        let file_root = tempfile::NamedTempFile::new().unwrap();
        diagnostic.record_id = None;
        save(file_root.path(), &mut diagnostic);
        assert!(diagnostic.record_id.is_none());
    }

    #[test]
    fn truncated_credential_lines_and_opaque_material_are_not_published() {
        let mut trace = Trace::default();
        trace.stderr = VecDeque::from(b"value-without-key\nnormal failure\n".to_vec());
        trace.truncated = true;
        assert_eq!(trace.snapshot(&[]).stderr_tail, "normal failure");
        let text = format!("private key=short\n{}\nnormal failure", "x".repeat(64));
        let redacted = redact(&text, &[]);
        assert!(!redacted.contains("short"));
        assert!(!redacted.contains(&"x".repeat(64)));
    }
}
