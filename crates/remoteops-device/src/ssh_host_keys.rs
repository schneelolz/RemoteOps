//! 主机密钥扫描与显式核对后的原子信任写入；扫描本身不产生信任。

use super::{
    BASE64, DeviceError, Digest, ProcessOutputEncoding, Sha256, SshHostKeyScan, SystemDevice,
    default_ssh_known_hosts_file, known_hosts_target, validate_ssh_destination,
};
use base64::Engine as _;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;

fn validate_target(host: &str, port: u16) -> Result<(), DeviceError> {
    if host.is_empty() || port == 0 {
        return Err(DeviceError::InvalidInput("SSH 主机与端口无效".to_owned()));
    }
    validate_ssh_destination(host, "scan")
}

fn key_fingerprint(kind: &str, encoded: &str) -> Result<String, DeviceError> {
    let invalid = || DeviceError::InvalidInput("SSH 主机公钥记录无效".to_owned());
    let key = BASE64.decode(encoded).map_err(|_| invalid())?;
    let length = key
        .get(..4)
        .and_then(|v| <[u8; 4]>::try_from(v).ok())
        .map(u32::from_be_bytes)
        .ok_or_else(invalid)?;
    let length = usize::try_from(length).map_err(|_| invalid())?;
    if length == 0
        || key.get(4..4_usize.saturating_add(length)) != Some(kind.as_bytes())
        || key.len() <= 4_usize.saturating_add(length)
    {
        return Err(invalid());
    }
    let fingerprint = base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(key));
    Ok(format!("{kind} SHA256:{fingerprint}"))
}

fn parse_scan(host: &str, port: u16, text: &str) -> Result<SshHostKeyScan, DeviceError> {
    validate_target(host, port)?;
    let target = known_hosts_target(host, port);
    let mut entries = Vec::new();
    let mut fingerprints = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty() && !v.starts_with('#'))
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[0] != target {
            return Err(DeviceError::InvalidInput(
                "SSH 扫描返回了其他目标或无效记录".to_owned(),
            ));
        }
        let fingerprint = key_fingerprint(fields[1], fields[2])?;
        if !fingerprints.contains(&fingerprint) {
            fingerprints.push(fingerprint);
            entries.push(format!("{} {} {}", fields[0], fields[1], fields[2]));
        }
    }
    if entries.is_empty() {
        return Err(DeviceError::Ssh {
            backend: "keyscan",
            category: "no_host_key",
        });
    }
    Ok(SshHostKeyScan {
        host: host.to_owned(),
        port,
        entries,
        fingerprints,
        diagnostics: None,
    })
}

fn scan_arguments(host: &str, port: u16) -> Vec<String> {
    vec![
        "-T".to_owned(),
        "5".to_owned(),
        "-p".to_owned(),
        port.to_string(),
        host.to_owned(),
    ]
}

fn scan_error(category: &'static str, exit_code: Option<i32>, stderr: String) -> DeviceError {
    DeviceError::SshScan {
        category,
        exit_code,
        stderr,
    }
}

fn sanitize_stderr(stderr: &str, host: &str) -> String {
    stderr
        .replace(host, "[target]")
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(1024)
        .collect()
}

/// 扫描主机公钥，不写信任；同步兼容入口与异步入口共用进程预算。
///
/// # Errors
/// 当目标、输出记录无效或扫描程序失败时返回错误。
pub fn scan_ssh_host_keys(
    host: &str,
    port: u16,
    timeout_seconds: u64,
) -> Result<SshHostKeyScan, DeviceError> {
    validate_target(host, port)?;
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| scan_error("runtime", None, String::new()))?
                    .block_on(scan_ssh_host_keys_async(host, port, timeout_seconds))
            })
            .join()
            .map_err(|_| scan_error("runtime", None, String::new()))?
    })
}

/// 受统一超时与取消管理的扫描；Windows 仅执行随包且摘要核验的固定绝对路径。
///
/// # Errors
/// 当目标无效、组件缺失、超时或返回记录无效时返回错误。
pub async fn scan_ssh_host_keys_async(
    host: &str,
    port: u16,
    timeout_seconds: u64,
) -> Result<SshHostKeyScan, DeviceError> {
    scan_ssh_host_keys_with_tools(host, port, timeout_seconds, None).await
}

/// 使用可信宿主提供的内嵌组件扫描，扫描本身不写信任。
///
/// # Errors
/// 目标、工具完整性、预算或扫描输出校验失败时返回错误。
pub async fn scan_ssh_host_keys_with_tools(
    host: &str,
    port: u16,
    timeout_seconds: u64,
    tools: Option<&super::BundledTools>,
) -> Result<SshHostKeyScan, DeviceError> {
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds.clamp(1, 15));
    scan_until(host, port, deadline, tools).await
}

async fn scan_until(
    host: &str,
    port: u16,
    deadline: tokio::time::Instant,
    tools: Option<&super::BundledTools>,
) -> Result<SshHostKeyScan, DeviceError> {
    validate_target(host, port)?;
    #[cfg(windows)]
    let guard = if let Some(tools) = tools {
        tools.prepare_ssh_scanner()?
    } else {
        super::bundled_tools::prepare_sidecar_ssh_scanner()?
    }
    .ok_or_else(|| scan_error("scanner_unavailable", None, String::new()))?;
    #[cfg(windows)]
    let executable = super::path_for_child_process(guard.path());
    #[cfg(not(windows))]
    let executable = {
        let _ = tools;
        "ssh-keyscan".to_owned()
    };
    scan_process(host, port, deadline, executable).await
}

async fn scan_process(
    host: &str,
    port: u16,
    deadline: tokio::time::Instant,
    executable: String,
) -> Result<SshHostKeyScan, DeviceError> {
    let process_deadline =
        deadline.min(tokio::time::Instant::now() + std::time::Duration::from_secs(12));
    if tokio::time::Instant::now() >= process_deadline {
        return Err(scan_error("timeout", None, String::new()));
    }
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let device = SystemDevice::new();
    let operation = device.run_process_streaming_with_encoding(
        executable,
        scan_arguments(host, port),
        12,
        sender,
        ProcessOutputEncoding::Utf8,
    );
    tokio::pin!(operation);
    let mut diagnostic = String::new();
    let mut stream_open = true;
    let result = loop {
        tokio::select! {
            result = &mut operation => break Some(result),
            () = tokio::time::sleep_until(process_deadline) => break None,
            chunk = receiver.recv(), if stream_open => {
                if let Some(chunk) = chunk {
                    if chunk.stderr && diagnostic.len() < 4096 {
                        diagnostic.extend(chunk.text.chars().take(1024));
                    }
                } else { stream_open = false; }
            }
        }
    };
    while let Ok(chunk) = receiver.try_recv() {
        if chunk.stderr && diagnostic.len() < 4096 {
            diagnostic.extend(chunk.text.chars().take(1024));
        }
    }
    let diagnostic = sanitize_stderr(&diagnostic, host);
    let output = match result {
        None | Some(Err(DeviceError::Timeout)) => {
            return Err(scan_error("timeout", None, diagnostic));
        }
        Some(Err(DeviceError::OutputLimit { .. })) => {
            return Err(scan_error("output_limit", None, diagnostic));
        }
        Some(Err(_)) => return Err(scan_error("spawn_or_io", None, diagnostic)),
        Some(Ok(output)) => output,
    };
    let stderr = sanitize_stderr(&output.stderr, host);
    if output.exit_code != Some(0) {
        return Err(scan_error("scanner_exit", output.exit_code, stderr));
    }
    let mut scan = parse_scan(host, port, &output.stdout).map_err(|error| match error {
        DeviceError::Ssh { category, .. } => scan_error(category, output.exit_code, stderr.clone()),
        _ => scan_error("invalid_output", output.exit_code, stderr.clone()),
    })?;
    if tokio::time::Instant::now() >= deadline {
        return Err(scan_error("timeout", output.exit_code, stderr));
    }
    scan.diagnostics = Some(super::SshScanDiagnostics {
        backend: if cfg!(windows) {
            "bundled_microsoft_openssh"
        } else {
            "system_openssh"
        },
        version: cfg!(windows).then_some(super::SSH_SCANNER_VERSION),
        exit_code: output.exit_code,
        stderr,
    });
    Ok(scan)
}

fn validate_expected(expected: &[String]) -> Result<(), DeviceError> {
    if expected.is_empty() || expected.len() > 32 {
        return Err(DeviceError::InvalidInput(
            "必须明确提供已核对的 SSH 指纹数组".to_owned(),
        ));
    }
    for fingerprint in expected {
        let Some((kind, digest)) = fingerprint.split_once(" SHA256:") else {
            return Err(DeviceError::InvalidInput(
                "SSH 指纹必须包含密钥类型与 SHA256".to_owned(),
            ));
        };
        if kind.is_empty()
            || kind.chars().any(char::is_whitespace)
            || !base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(digest)
                .is_ok_and(|v| v.len() == 32)
        {
            return Err(DeviceError::InvalidInput("SSH 指纹格式无效".to_owned()));
        }
    }
    Ok(())
}

fn verify_expected(scan: &SshHostKeyScan, expected: &[String]) -> Result<(), DeviceError> {
    validate_expected(expected)?;
    let actual = scan.fingerprints.iter().collect::<BTreeSet<_>>();
    let expected = expected.iter().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(DeviceError::Ssh {
            backend: "keyscan",
            category: "fingerprint_changed",
        });
    }
    Ok(())
}

fn open_trust_lock(path: &Path) -> Result<std::fs::File, DeviceError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
    }
    // 锁文件不删除，避免等待者仍持有旧 inode 时另一进程创建第二把锁。
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|_| DeviceError::Operation("无法打开 SSH 信任库跨进程锁".into()))
}

fn try_trust_lock(file: &std::fs::File) -> Result<bool, DeviceError> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(_)) => {
            Err(DeviceError::Operation("SSH 信任库跨进程锁失败".into()))
        }
    }
}

fn validate_scan(scan: &SshHostKeyScan) -> Result<(), DeviceError> {
    let validated = parse_scan(&scan.host, scan.port, &scan.entries.join("\n"))?;
    verify_expected(&validated, &scan.fingerprints)
}

fn persist_scan(scan: &SshHostKeyScan, path: &Path) -> Result<PathBuf, DeviceError> {
    validate_scan(scan)?;
    let lock = open_trust_lock(path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if try_trust_lock(&lock)? {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(DeviceError::Ssh {
                backend: "trust",
                category: "lock_timeout",
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    persist_scan_locked(
        scan,
        path,
        std::time::Instant::now() + std::time::Duration::from_secs(15),
    )
}

async fn persist_scan_async(
    scan: &SshHostKeyScan,
    path: &Path,
    deadline: tokio::time::Instant,
) -> Result<PathBuf, DeviceError> {
    check_trust_deadline(deadline.into_std())?;
    validate_scan(scan)?;
    check_trust_deadline(deadline.into_std())?;
    let lock = open_trust_lock(path)?;
    check_trust_deadline(deadline.into_std())?;
    let lock_deadline =
        deadline.min(tokio::time::Instant::now() + std::time::Duration::from_secs(2));
    loop {
        if tokio::time::Instant::now() >= lock_deadline {
            return Err(DeviceError::Ssh {
                backend: "trust",
                category: "lock_timeout",
            });
        }
        if try_trust_lock(&lock)? {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    if tokio::time::Instant::now() >= deadline {
        return Err(scan_error("timeout", None, String::new()));
    }
    // 获锁后无 await，也不启动后台写入；取消只能在提交前赢，提交后返回实际结果。
    persist_scan_locked(scan, path, deadline.into_std())
}

fn check_trust_deadline(deadline: std::time::Instant) -> Result<(), DeviceError> {
    if std::time::Instant::now() >= deadline {
        return Err(DeviceError::Ssh {
            backend: "trust",
            category: "timeout",
        });
    }
    Ok(())
}

fn persist_scan_locked(
    scan: &SshHostKeyScan,
    path: &Path,
    deadline: std::time::Instant,
) -> Result<PathBuf, DeviceError> {
    persist_scan_locked_before_commit(scan, path, deadline, || {})
}

fn persist_scan_locked_before_commit(
    scan: &SshHostKeyScan,
    path: &Path,
    deadline: std::time::Instant,
    before_commit: impl FnOnce(),
) -> Result<PathBuf, DeviceError> {
    check_trust_deadline(deadline)?;
    let validated = parse_scan(&scan.host, scan.port, &scan.entries.join("\n"))?;
    check_trust_deadline(deadline)?;
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(DeviceError::Operation(error.to_string())),
    };
    check_trust_deadline(deadline)?;
    let target = known_hosts_target(&scan.host, scan.port);
    let mut lines = Vec::new();
    for line in existing.lines() {
        check_trust_deadline(deadline)?;
        let fields = line.split_whitespace().collect::<Vec<_>>();
        // 标记记录原样保留（特别是撤销），仅替换该目标的普通精确记录。
        if fields
            .first()
            .is_none_or(|first| first.starts_with('@') || first.starts_with('#'))
            || !fields[0].split(',').any(|host| host == target)
        {
            lines.push(line.to_owned());
            continue;
        }
        let remaining = fields[0]
            .split(',')
            .filter(|host| *host != target)
            .collect::<Vec<_>>()
            .join(",");
        if !remaining.is_empty() {
            lines.push(format!("{} {}", remaining, fields[1..].join(" ")));
        }
    }
    lines.extend(validated.entries);
    check_trust_deadline(deadline)?;
    let temporary = path.with_extension(format!("{}.tmp", remoteops_domain::RequestId::new()));
    let mut temporary_created = false;
    let result: Result<(), DeviceError> = (|| {
        use std::io::Write as _;
        check_trust_deadline(deadline)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        temporary_created = true;
        check_trust_deadline(deadline)?;
        let content = format!("{}\n", lines.join("\n"));
        check_trust_deadline(deadline)?;
        for chunk in content.as_bytes().chunks(65_536) {
            check_trust_deadline(deadline)?;
            file.write_all(chunk)
                .map_err(|error| DeviceError::Operation(error.to_string()))?;
            check_trust_deadline(deadline)?;
        }
        file.sync_all()
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        check_trust_deadline(deadline)?;
        drop(file);
        before_commit();
        check_trust_deadline(deadline)?;
        #[cfg(windows)]
        super::ssh_atomic_file::replace_until(&temporary, path, deadline)?;
        #[cfg(not(windows))]
        {
            check_trust_deadline(deadline)?;
            std::fs::rename(&temporary, path)
                .map_err(|_| DeviceError::Operation("SSH 信任库原子替换失败".into()))?;
        }
        Ok(())
    })();
    if result.is_err() && temporary_created {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(path.to_owned())
}

/// 将已经由现场用户明确确认的扫描结果写入专用信任文件。
///
/// # Errors
/// 当目标／密钥记录不一致或文件无法原子更新时返回错误。
pub fn trust_ssh_host_keys(scan: &SshHostKeyScan) -> Result<PathBuf, DeviceError> {
    persist_scan(scan, &default_ssh_known_hosts_file()?)
}

/// 重扫并严格匹配现场明确核对的完整指纹集合后，才原子更新目标信任。
///
/// # Errors
/// 当指纹未明确提供、发生变化、扫描超时或文件无法更新时返回错误。
pub async fn trust_ssh_host_keys_verified(
    host: &str,
    port: u16,
    expected_fingerprints: &[String],
    timeout_seconds: u64,
) -> Result<PathBuf, DeviceError> {
    trust_ssh_host_keys_verified_with_tools(
        host,
        port,
        expected_fingerprints,
        timeout_seconds,
        None,
    )
    .await
}

/// 使用可信扫描组件重扫，并在同一总预算内核对及原子写入。
///
/// # Errors
/// 指纹变化、组件不完整、超时或写入失败时拒绝更新信任。
pub async fn trust_ssh_host_keys_verified_with_tools(
    host: &str,
    port: u16,
    expected_fingerprints: &[String],
    timeout_seconds: u64,
    tools: Option<&super::BundledTools>,
) -> Result<PathBuf, DeviceError> {
    // 无效确认不触发网络扫描，更不能变成自动首次信任。
    validate_expected(expected_fingerprints)?;
    let overall_deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds.clamp(1, 15));
    let scan = scan_until(host, port, overall_deadline, tools).await?;
    verify_expected(&scan, expected_fingerprints)?;
    check_trust_deadline(overall_deadline.into_std())?;
    persist_scan_async(&scan, &default_ssh_known_hosts_file()?, overall_deadline).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(host: &str, body: &[u8]) -> String {
        let key = [&11_u32.to_be_bytes()[..], b"ssh-ed25519", body].concat();
        format!("{host} ssh-ed25519 {}", BASE64.encode(key))
    }
    #[test]
    fn scanner_budget_and_diagnostics_are_bounded_before_parsing() {
        assert_eq!(
            scan_arguments("target", 2222),
            ["-T", "5", "-p", "2222", "target"]
        );
        let text = format!("secret-host \u{1b}[31m {}", "x".repeat(3000));
        let diagnostic = sanitize_stderr(&text, "secret-host");
        assert!(!diagnostic.contains("secret-host"));
        assert!(!diagnostic.contains('\u{1b}'));
        assert!(diagnostic.chars().count() <= 1024);
    }

    #[test]
    fn scans_are_exact_and_cannot_be_retargeted_or_trusted_without_expected() {
        let good = entry("example", b"fixture-key");
        let mut scan = parse_scan("example", 22, &good).unwrap();
        assert!(parse_scan("other", 22, &good).is_err());
        assert!(parse_scan("example", 2222, &good).is_err());
        assert!(parse_scan("example", 22, &good.replace("ssh-ed25519 ", "ssh-rsa ")).is_err());
        assert!(verify_expected(&scan, &[]).is_err());
        assert!(verify_expected(&scan, &["arbitrary".to_owned()]).is_err());
        assert!(verify_expected(&scan, &scan.fingerprints).is_ok());
        let changed = parse_scan("example", 22, &entry("example", b"changed-key")).unwrap();
        assert!(verify_expected(&changed, &scan.fingerprints).is_err());
        scan.host = "other".to_owned();
        let path = std::env::temp_dir().join(format!(
            "remoteops-trust-invalid-{}",
            remoteops_domain::RequestId::new()
        ));
        assert!(persist_scan(&scan, &path).is_err());
        assert!(!path.exists());
    }
    #[test]
    fn atomic_trust_replaces_only_target_and_keeps_aliases_and_revocation() {
        let directory = std::env::temp_dir().join(format!(
            "remoteops-trust-{}",
            remoteops_domain::RequestId::new()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("known_hosts");
        let old = entry("example,alias", b"old");
        let revoked = format!("@revoked {}", entry("other", b"revoked"));
        std::fs::write(&path, format!("{old}\n{revoked}\n")).unwrap();
        let scan = parse_scan("example", 22, &entry("example", b"new")).unwrap();
        persist_scan(&scan, &path).unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains(&entry("alias", b"old")));
        assert!(result.contains(&revoked));
        assert!(result.contains(&entry("example", b"new")));
        assert!(!result.contains("example,alias"));
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn expired_commit_budget_keeps_old_trust_and_removes_prepared_temporary() {
        let directory = std::env::temp_dir().join(format!(
            "remoteops-trust-deadline-{}",
            remoteops_domain::RequestId::new()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("known_hosts");
        std::fs::write(&path, b"# original trust\n").unwrap();
        let scan = parse_scan("example", 22, &entry("example", b"new-key")).unwrap();
        let expired = std::time::Instant::now();
        assert!(persist_scan_locked(&scan, &path, expired).is_err());
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(40);
        let result = persist_scan_locked_before_commit(&scan, &path, deadline, || {
            assert!(std::fs::read_dir(&directory).unwrap().count() > 1);
            std::thread::sleep(
                deadline.saturating_duration_since(std::time::Instant::now())
                    + std::time::Duration::from_millis(5),
            );
        });
        assert!(matches!(
            result,
            Err(DeviceError::Ssh {
                backend: "trust",
                category: "timeout"
            })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"# original trust\n");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        assert!(
            persist_scan_locked(
                &scan,
                &path,
                std::time::Instant::now() + std::time::Duration::from_secs(2)
            )
            .is_ok()
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("example ssh-ed25519")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn trust_process_fixture() {
        let Some(directory) = std::env::var_os("REMOTEOPS_TEST_TRUST_DIRECTORY") else {
            return;
        };
        let directory = PathBuf::from(directory);
        assert!(directory.starts_with(std::env::temp_dir()));
        assert!(
            directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("remoteops-trust-process-")
        );
        let host = std::env::var("REMOTEOPS_TEST_TRUST_HOST").unwrap();
        assert!(matches!(host.as_str(), "first" | "second"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !directory.join("start").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for index in 0..10 {
            let scan =
                parse_scan(&host, 22, &entry(&host, format!("key-{index}").as_bytes())).unwrap();
            persist_scan(&scan, &directory.join("known_hosts")).unwrap();
        }
    }

    fn writer_succeeded(child: std::process::Child) -> bool {
        let output = child.wait_with_output().unwrap();
        for line in String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("trust_atomic_os_error="))
        {
            eprintln!("{line}");
        }
        output.status.success()
    }

    #[test]
    fn concurrent_process_trust_keeps_both_targets_and_never_removes_the_live_file() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let directory = std::env::temp_dir().join(format!(
            "remoteops-trust-process-{}",
            remoteops_domain::RequestId::new()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("known_hosts");
        let original = format!(
            "{}\n@revoked {}\n",
            entry("first,alias", b"original"),
            entry("revoked", b"denied")
        );
        std::fs::write(&path, &original).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let healthy = Arc::new(AtomicBool::new(true));
        let failures = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
            String,
            usize,
        >::new()));
        let reader = {
            let path = path.clone();
            let stop = stop.clone();
            let healthy = healthy.clone();
            let failures = failures.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let failure = match std::fs::read_to_string(&path) {
                        Ok(text)
                            if text.ends_with('\n')
                                && text.contains("alias")
                                && text.contains("@revoked") =>
                        {
                            None
                        }
                        Ok(_) => Some("partial_content".to_owned()),
                        Err(error) => {
                            Some(format!("{:?}/os={:?}", error.kind(), error.raw_os_error()))
                        }
                    };
                    if let Some(category) = failure {
                        *failures.lock().unwrap().entry(category).or_default() += 1;
                        healthy.store(false, Ordering::Release);
                    }
                    std::thread::yield_now();
                }
            })
        };
        let children = ["first", "second"]
            .into_iter()
            .map(|host| {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "ssh_host_keys::tests::trust_process_fixture",
                        "--nocapture",
                    ])
                    .env("REMOTEOPS_TEST_TRUST_DIRECTORY", &directory)
                    .env("REMOTEOPS_TEST_TRUST_HOST", host)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt as _;
                    command.creation_flags(0x0800_0000);
                }
                command.spawn().unwrap()
            })
            .collect::<Vec<_>>();
        std::fs::write(directory.join("start"), []).unwrap();
        let successes = children
            .into_iter()
            .map(writer_succeeded)
            .collect::<Vec<_>>();
        stop.store(true, Ordering::Release);
        reader.join().unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        eprintln!("trust_reader_error_counts={:?}", failures.lock().unwrap());
        assert!(successes.into_iter().all(|success| success));
        assert!(
            healthy.load(Ordering::Acquire),
            "严格读者遇到缺失、访问错误或半文件；固定类别计数见上方"
        );
        assert!(result.contains(&entry("alias", b"original")));
        assert!(result.contains(&format!("@revoked {}", entry("revoked", b"denied"))));
        assert!(result.contains(&entry("first", b"key-9")));
        assert!(result.contains(&entry("second", b"key-9")));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn cancelled_or_expired_lock_wait_cannot_publish_later() {
        let directory = std::env::temp_dir().join(format!(
            "remoteops-trust-cancel-{}",
            remoteops_domain::RequestId::new()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("known_hosts");
        std::fs::write(&path, "# unchanged\n").unwrap();
        let lock = open_trust_lock(&path).unwrap();
        assert!(try_trust_lock(&lock).unwrap());
        let scan = parse_scan("example", 22, &entry("example", b"key")).unwrap();
        let expired = persist_scan_async(
            &scan,
            &path,
            tokio::time::Instant::now() + std::time::Duration::from_millis(30),
        )
        .await;
        assert!(matches!(
            expired,
            Err(DeviceError::Ssh {
                category: "lock_timeout",
                ..
            })
        ));
        let task = {
            let path = path.clone();
            let scan = scan.clone();
            tokio::spawn(async move {
                persist_scan_async(
                    &scan,
                    &path,
                    tokio::time::Instant::now() + std::time::Duration::from_secs(2),
                )
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(lock);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# unchanged\n");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn verified_trust_rejects_empty_confirmation_before_network() {
        assert!(
            trust_ssh_host_keys_verified("invalid", 0, &[], 1)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn scanner_timeout_and_cancellation_are_bounded_on_local_silent_peer() {
        use tokio::io::AsyncReadExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (connected, accepted) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn(async move {
            let first = listener.accept().await.unwrap().0;
            let _ = connected.send(first);
            let mut sockets = Vec::new();
            loop {
                sockets.push(listener.accept().await.unwrap().0);
            }
        });
        let executable = if cfg!(windows) {
            format!(
                "{}/System32/OpenSSH/ssh-keyscan.exe",
                std::env::var("SystemRoot").unwrap()
            )
        } else {
            "ssh-keyscan".into()
        };
        let task = tokio::spawn(scan_process(
            "127.0.0.1",
            port,
            tokio::time::Instant::now() + std::time::Duration::from_secs(15),
            executable.clone(),
        ));
        let mut socket = tokio::time::timeout(std::time::Duration::from_secs(5), accepted)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        // 真正启动过的 keyscan 取消后，其连接必须关闭，而不只是 Rust future 已结束。
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut buffer = [0_u8; 1024];
            loop {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await
        .unwrap();
        let started = std::time::Instant::now();
        assert!(
            scan_process(
                "127.0.0.1",
                port,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
                executable
            )
            .await
            .is_err()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        holder.abort();
    }
}
