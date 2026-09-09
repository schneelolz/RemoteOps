//! Windows 交互式 Provider 的安全边界。
//!
//! 真实 UIA 适配器通过本模块的命令通道接入；Agent Service 只负责创建用户
//! Session 中的 Provider，不直接调用桌面 API。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

/// Provider 与 Agent 之间的 Named Pipe 命令。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderCommand {
    Observe {
        screenshot: bool,
        ui_tree: bool,
    },
    WaitFor {
        condition: String,
        timeout_millis: u64,
    },
    Invoke {
        target_fingerprint: String,
        action: String,
    },
    TypeText {
        target_fingerprint: String,
        text: String,
    },
    SendInput {
        target_fingerprint: String,
        input: String,
        approved: bool,
    },
    Stop,
}

/// 锁定版本的 Windows-MCP 启动配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsMcpConfig {
    pub executable: PathBuf,
    pub sha256: String,
}

impl WindowsMcpConfig {
    /// 拒绝空哈希及相对路径，避免执行未锁定的 Provider。
    ///
    /// # Errors
    /// 当路径不是绝对路径或哈希不是十六进制 SHA-256 时返回错误。
    pub fn validate(&self) -> Result<(), String> {
        if !self.executable.is_absolute() {
            return Err("Windows-MCP 路径必须是绝对路径".into());
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Windows-MCP 必须配置 64 位 SHA-256".into());
        }
        Ok(())
    }

    /// 验证磁盘文件的 SHA-256 与发布时锁定值一致。
    ///
    /// # Errors
    /// 文件无法读取或摘要不匹配时返回错误。
    pub fn verify_file(&self) -> Result<(), String> {
        self.validate()?;
        let bytes =
            std::fs::read(&self.executable).map_err(|e| format!("无法读取 Windows-MCP：{e}"))?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        if !digest.eq_ignore_ascii_case(&self.sha256) {
            return Err("Windows-MCP 文件 SHA-256 不匹配".into());
        }
        Ok(())
    }
}

/// Windows-MCP 子进程监管器；Provider 崩溃后只允许显式重启并留下审计日志。
pub struct WindowsMcpSupervisor {
    config: WindowsMcpConfig,
    pipe_name: String,
    child: Option<Child>,
}

impl WindowsMcpSupervisor {
    /// 从 Provider 环境读取锁定版本配置；未启用外部 Provider 时返回 `None`。
    ///
    /// # Errors
    /// 外部 Provider 已启用但路径、摘要或 Pipe 配置缺失或无效时返回错误。
    pub fn from_environment() -> Result<Option<Self>, String> {
        if !std::env::var("REMOTEOPS_WINDOWS_MCP_ENABLED")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        {
            return Ok(None);
        }
        let executable = std::env::var_os("REMOTEOPS_WINDOWS_MCP_PATH")
            .ok_or_else(|| "已启用 Windows-MCP，但未配置 REMOTEOPS_WINDOWS_MCP_PATH".to_owned())?;
        let sha256 = std::env::var("REMOTEOPS_WINDOWS_MCP_SHA256")
            .map_err(|_| "已启用 Windows-MCP，但未配置 REMOTEOPS_WINDOWS_MCP_SHA256".to_owned())?;
        let pipe_name =
            std::env::var("REMOTEOPS_WINDOWS_MCP_PIPE").unwrap_or_else(|_| default_pipe_name());
        Self::new(
            WindowsMcpConfig {
                executable: executable.into(),
                sha256,
            },
            pipe_name,
        )
        .map(Some)
    }

    /// 创建监管器并校验用户 Session 专属 Pipe 名称。
    ///
    /// # Errors
    /// 配置或 Pipe 名称无效时返回错误。
    pub fn new(config: WindowsMcpConfig, pipe_name: impl Into<String>) -> Result<Self, String> {
        let pipe_name = pipe_name.into();
        config.validate()?;
        validate_pipe_name(&pipe_name)?;
        Ok(Self {
            config,
            pipe_name,
            child: None,
        })
    }

    /// 启动锁定版本的 Windows-MCP，并将其标准流隔离。
    ///
    /// # Errors
    /// 文件摘要不匹配或子进程无法启动时返回错误。
    pub fn start(&mut self) -> Result<(), String> {
        self.config.verify_file()?;
        if self.is_running() {
            return Ok(());
        }
        let mut command = Command::new(&self.config.executable);
        command
            .arg("--pipe")
            .arg(&self.pipe_name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let child = command
            .spawn()
            .map_err(|e| format!("启动 Windows-MCP 失败：{e}"))?;
        self.child = Some(child);
        tracing::info!(pipe = %self.pipe_name, "Windows-MCP provider started");
        Ok(())
    }

    /// 判断受监管子进程是否仍在运行。
    pub fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
    }

    /// 停止子进程并记录生命周期事件。
    pub async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            tracing::info!(pipe = %self.pipe_name, "Windows-MCP provider stopped");
        }
    }

    /// 只在调用方重新确认配置后重启 Provider。
    ///
    /// # Errors
    /// 新进程无法通过摘要验证或启动时返回错误。
    pub async fn restart(&mut self) -> Result<(), String> {
        self.stop().await;
        self.start()
    }

    /// 向受监管的 Windows-MCP 发送一条带换行分隔的 JSON 请求。
    ///
    /// Named Pipe 连接只在子进程已启动且 Pipe 名称通过校验时建立；连接或响应
    /// 超时会返回明确错误，调用方必须把该错误写入审计并停止后续图形动作。
    ///
    /// # Errors
    /// 子进程未运行、Pipe 无法连接、读写超时或响应不是有效 JSON 时返回错误。
    pub async fn request(
        &mut self,
        command: &ProviderCommand,
    ) -> Result<serde_json::Value, String> {
        if !self.is_running() {
            return Err("Windows-MCP Provider 未运行，拒绝通过 Named Pipe 请求".into());
        }
        let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&self.pipe_name)
            .map_err(|error| format!("连接 Windows-MCP Named Pipe 失败：{error}"))?;
        let mut payload = serde_json::to_vec(command)
            .map_err(|error| format!("序列化 Windows-MCP 请求失败：{error}"))?;
        payload.push(b'\n');
        timeout(Duration::from_secs(10), pipe.write_all(&payload))
            .await
            .map_err(|_| "写入 Windows-MCP Named Pipe 超时".to_owned())?
            .map_err(|error| format!("写入 Windows-MCP Named Pipe 失败：{error}"))?;
        let mut line = String::new();
        timeout(
            Duration::from_secs(30),
            BufReader::new(&mut pipe).read_line(&mut line),
        )
        .await
        .map_err(|_| "读取 Windows-MCP Named Pipe 超时".to_owned())?
        .map_err(|error| format!("读取 Windows-MCP Named Pipe 失败：{error}"))?;
        serde_json::from_str(line.trim())
            .map_err(|error| format!("Windows-MCP 返回无效 JSON：{error}"))
    }
}

fn default_pipe_name() -> String {
    format!(r"\\.\pipe\RemoteOps-windows-mcp-{}", std::process::id())
}

/// 交互式 Session 的安全门禁，供 UIA 和输入后端共同使用。
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DesktopSafety {
    pub interactive_session: bool,
    pub secure_desktop: bool,
    pub foreground_window: bool,
    pub integrity_match: bool,
    pub uia_available: bool,
}

impl DesktopSafety {
    /// 输入只能在登录用户、普通桌面、前台窗口和权限级别一致时执行。
    ///
    /// # Errors
    /// 当桌面处于锁屏、Secure Desktop、无前台窗口、权限不匹配，或未获回退审批时返回错误。
    pub fn allow_input(&self, coordinate_fallback_approved: bool) -> Result<(), String> {
        if !self.interactive_session {
            return Err("没有交互式 Windows Session".into());
        }
        if self.secure_desktop {
            return Err("Secure Desktop 上禁止输入".into());
        }
        if !self.foreground_window {
            return Err("没有可验证的前台窗口".into());
        }
        if !self.integrity_match {
            return Err("窗口权限级别不匹配".into());
        }
        if !self.uia_available && !coordinate_fallback_approved {
            return Err("UI Automation 不可用且坐标回退未获审批".into());
        }
        Ok(())
    }
}

/// Named Pipe 名称必须局限于当前用户的 `RemoteOps` 命名空间。
///
/// # Errors
/// 当名称不以受保护的前缀开头或超过长度限制时返回错误。
pub fn validate_pipe_name(name: &str) -> Result<(), String> {
    if !name.starts_with(r"\\.\pipe\RemoteOps-") || name.len() > 256 {
        return Err("Named Pipe 名称不在 RemoteOps 用户命名空间内".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safety_rejects_secure_desktop_and_unapproved_fallback() {
        let mut s = DesktopSafety {
            interactive_session: true,
            foreground_window: true,
            integrity_match: true,
            ..Default::default()
        };
        s.secure_desktop = true;
        assert!(s.allow_input(false).is_err());
        s.secure_desktop = false;
        assert!(s.allow_input(false).is_err());
        s.uia_available = true;
        assert!(s.allow_input(false).is_ok());
    }
    #[test]
    fn config_and_pipe_are_strictly_validated() {
        assert!(validate_pipe_name(r"\\.\pipe\RemoteOps-user").is_ok());
        assert!(validate_pipe_name(r"\\.\pipe\Other-user").is_err());
        let c = WindowsMcpConfig {
            executable: PathBuf::from(r"C:\RemoteOps\windows-mcp.exe"),
            sha256: "a".repeat(64),
        };
        assert!(c.validate().is_ok());
        assert!(validate_pipe_name(&default_pipe_name()).is_ok());
    }
}
