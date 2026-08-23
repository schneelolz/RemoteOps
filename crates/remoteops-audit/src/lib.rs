//! 审计文本脱敏和文件哈希。

use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use regex::Regex;
use remoteops_domain::{AuditEvent, SessionId};
use sha2::{Digest, Sha256};

/// 日志和审计输出使用的脱敏器。
pub struct Redactor {
    rules: Vec<Regex>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self {
            rules: vec![
                Regex::new(r"(?i)(api[_-]?key|token|password|passwd|cookie|session)\s*[:=]\s*([^\s,;]+)")
                    .expect("内置脱敏正则必须有效"),
                Regex::new(r"(?i)(authorization\s*:\s*bearer)\s+[A-Za-z0-9._~+/=-]+")
                    .expect("内置脱敏正则必须有效"),
                Regex::new(r"(?i)\b(sk-[A-Za-z0-9_-]{12,})\b")
                    .expect("内置脱敏正则必须有效"),
                Regex::new(
                    r#"(?i)(--?(?:password|passwd|token|api[_-]?key|secret)|/(?:password|token))\s+("[^"]*"|'[^']*'|[^\s,;]+)"#,
                )
                .expect("内置脱敏正则必须有效"),
                Regex::new(r"(?i)(://[^:/\s]+:)([^@\s]+)(@)")
                    .expect("内置脱敏正则必须有效"),
                Regex::new(
                    r"(?is)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                )
                .expect("内置脱敏正则必须有效"),
                Regex::new(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")
                    .expect("内置脱敏正则必须有效"),
                Regex::new(
                    r"(?i)\b(AccountKey|SharedAccessSignature|ClientSecret|PrivateKey)\s*=\s*([^;\s]+)",
                )
                .expect("内置脱敏正则必须有效"),
            ],
        }
    }
}

impl Redactor {
    /// 返回脱敏后的文本。
    #[must_use]
    pub fn redact(&self, input: &str) -> String {
        let mut value = input.to_owned();
        value = self.rules[0]
            .replace_all(&value, "$1=[REDACTED]")
            .into_owned();
        value = self.rules[1]
            .replace_all(&value, "$1 [REDACTED]")
            .into_owned();
        value = self.rules[2].replace_all(&value, "[REDACTED]").into_owned();
        value = self.rules[3]
            .replace_all(&value, "$1 [REDACTED]")
            .into_owned();
        value = self.rules[4]
            .replace_all(&value, "$1[REDACTED]$3")
            .into_owned();
        value = self.rules[5]
            .replace_all(&value, "[REDACTED PRIVATE KEY]")
            .into_owned();
        value = self.rules[6]
            .replace_all(&value, "[REDACTED JWT]")
            .into_owned();
        self.rules[7]
            .replace_all(&value, "$1=[REDACTED]")
            .into_owned()
    }
}

/// 计算字节数组的 SHA-256。
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 流式计算文件 SHA-256。
///
/// # Errors
///
/// 当文件无法打开或读取时返回底层 I/O 错误。
pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// 返回当前用户范围内的默认审计日志路径。
#[must_use]
pub fn default_audit_log_path() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("RemoteOps")
            .join("audit.jsonl");
    }
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state_home)
            .join("remoteops")
            .join("audit.jsonl");
    }
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(user_profile)
            .join(".remoteops")
            .join("audit.jsonl");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("RemoteOps")
            .join("audit.jsonl");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("remoteops")
            .join("audit.jsonl");
    }
    std::env::temp_dir().join("remoteops").join("audit.jsonl")
}

/// 以 JSON Lines 持久化脱敏审计事件，并支持按会话导出。
pub struct JsonlAuditStore {
    /// 审计源文件路径。
    path: PathBuf,
    /// 写入和导出期间的进程内互斥锁。
    lock: Mutex<()>,
    /// 写入前使用的敏感文本脱敏器。
    redactor: Redactor,
}

impl JsonlAuditStore {
    /// 创建审计存储；文件在首次写入时创建。
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
            redactor: Redactor::default(),
        }
    }

    /// 返回审计源文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一个已经结构化的审计事件，文本字段会再次脱敏。
    ///
    /// # Errors
    ///
    /// 当审计目录或文件无法创建、序列化或写入时返回错误。
    pub fn append(&self, event: &AuditEvent) -> io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("审计文件锁已损坏"))?;
        ensure_parent(&self.path)?;
        let mut event = event.clone();
        event.action = self.redactor.redact(&event.action);
        event.result = self.redactor.redact(&event.result);
        let line = serde_json::to_string(&event).map_err(io::Error::other)?;
        let mut file = open_private_append(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    }

    /// 将指定会话的审计事件导出为格式化 JSON 数组。
    ///
    /// # Errors
    ///
    /// 当源审计无法读取、目标文件无法创建或事件无法序列化时返回错误。
    pub fn export_session(
        &self,
        session_id: SessionId,
        destination: impl AsRef<Path>,
    ) -> io::Result<usize> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("审计文件锁已损坏"))?;
        let destination = destination.as_ref();
        if destination == self.path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "审计导出目标不能覆盖源文件",
            ));
        }
        let events = self.read_session_unlocked(session_id)?;
        ensure_parent(destination)?;
        let bytes = serde_json::to_vec_pretty(&events).map_err(io::Error::other)?;
        let mut file = create_private_file(destination)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(events.len())
    }

    fn read_session_unlocked(&self, session_id: SessionId) -> io::Result<Vec<AuditEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str::<AuditEvent>(&line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("审计文件第 {} 行无效：{error}", index + 1),
                )
            })?;
            if event.session_id == session_id {
                events.push(event);
            }
        }
        Ok(events)
    }
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
        set_private_directory_permissions(parent)?;
    }
    Ok(())
}

fn open_private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    set_private_create_mode(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    set_private_create_mode(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

#[cfg(unix)]
fn set_private_create_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_create_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn set_private_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn set_private_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use remoteops_domain::{
        ApprovalState, ControllerInstanceId, ControllerOwnerId, EventSource, PermissionMode,
        RequestId,
    };

    use super::*;

    #[test]
    fn redacts_common_secret_forms() {
        let redactor = Redactor::default();
        let input = "password=hunter2 Authorization: Bearer abc.def api_key:sk-example123456 --password 'quoted secret' https://user:url-secret@example.test AccountKey=storage-secret eyJaaaaaaaaaaa.bbbbbbbbbbb.ccccccccccc -----BEGIN PRIVATE KEY-----\nprivate-material\n-----END PRIVATE KEY-----";
        let output = redactor.redact(input);

        assert!(!output.contains("hunter2"));
        assert!(!output.contains("abc.def"));
        assert!(!output.contains("sk-example123456"));
        assert!(!output.contains("quoted secret"));
        assert!(!output.contains("url-secret"));
        assert!(!output.contains("storage-secret"));
        assert!(!output.contains("eyJaaaaaaaaaaa"));
        assert!(!output.contains("private-material"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn hashes_known_value() {
        assert_eq!(
            sha256_bytes(b"remoteops"),
            "2824337d3ca6fe3a9498bf56e1117c3f360eaacc3288eb8647c7bb035b4e7e48"
        );
    }

    #[test]
    fn persists_redacted_events_and_exports_one_session() {
        let test_root = std::env::temp_dir().join(format!(
            "remoteops-audit-{}",
            remoteops_domain::SessionId::new()
        ));
        let log_path = test_root.join("audit.jsonl");
        let export_path = test_root.join("export.json");
        let store = JsonlAuditStore::new(&log_path);
        let first_session = SessionId::new();
        let second_session = SessionId::new();
        for session_id in [first_session, second_session] {
            store
                .append(&AuditEvent {
                    session_id,
                    request_id: Some(RequestId::new()),
                    owner_id: Some(ControllerOwnerId::new()),
                    controller_instance_id: Some(ControllerInstanceId::new()),
                    source: EventSource::Ai,
                    action: "run_command password=secret".to_owned(),
                    result: "token=example".to_owned(),
                    approval: ApprovalState::NotRequired,
                    permission_mode: PermissionMode::FullAccess,
                    occurred_at: Utc::now(),
                })
                .expect("审计事件应写入");
        }

        let count = store
            .export_session(first_session, &export_path)
            .expect("应导出指定会话");
        let source = std::fs::read_to_string(&log_path).expect("应读取审计源文件");
        let export = std::fs::read_to_string(&export_path).expect("应读取导出文件");

        assert_eq!(count, 1);
        assert!(!source.contains("secret"));
        assert!(!source.contains("example"));
        assert!(export.contains(&first_session.to_string()));
        assert!(!export.contains(&second_session.to_string()));
        let _ = std::fs::remove_dir_all(test_root);
    }

    #[test]
    fn reads_legacy_events_without_owner_fields() {
        let session_id = SessionId::new();
        let json = format!(
            r#"{{"session_id":"{session_id}","request_id":null,"source":"system","action":"legacy","result":"ok","approval":"not_required","occurred_at":"2026-08-06T00:00:00Z"}}"#
        );

        let event: AuditEvent = serde_json::from_str(&json).expect("旧审计记录应保持可读取");

        assert_eq!(event.owner_id, None);
        assert_eq!(event.controller_instance_id, None);
        assert_eq!(event.permission_mode, PermissionMode::ApprovalRequired);
    }
}
