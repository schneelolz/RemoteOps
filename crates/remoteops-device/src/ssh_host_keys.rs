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
    })
}

fn scanner() -> &'static str {
    if cfg!(windows) {
        "ssh-keyscan.exe"
    } else {
        "ssh-keyscan"
    }
}

fn scan_arguments(host: &str, port: u16, seconds: u64) -> Vec<String> {
    vec![
        "-T".to_owned(),
        seconds.clamp(1, 30).to_string(),
        "-p".to_owned(),
        port.to_string(),
        host.to_owned(),
    ]
}

/// 扫描主机公钥，不写信任；旧同步入口保留兼容，新流程应使用异步入口。
///
/// # Errors
/// 当目标、输出记录无效或扫描程序失败时返回错误。
pub fn scan_ssh_host_keys(
    host: &str,
    port: u16,
    timeout_seconds: u64,
) -> Result<SshHostKeyScan, DeviceError> {
    validate_target(host, port)?;
    let mut command = std::process::Command::new(scanner());
    command.args(scan_arguments(host, port, timeout_seconds));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().map_err(|_| DeviceError::Ssh {
        backend: "keyscan",
        category: "spawn",
    })?;
    parse_scan(host, port, &String::from_utf8_lossy(&output.stdout))
}

/// 受统一超时与取消管理的主机密钥扫描；未知密钥不因此自动受信。
///
/// # Errors
/// 当目标无效、扫描超时、程序不可用或返回记录无效时返回错误。
pub async fn scan_ssh_host_keys_async(
    host: &str,
    port: u16,
    timeout_seconds: u64,
) -> Result<SshHostKeyScan, DeviceError> {
    validate_target(host, port)?;
    let (sender, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    let output = SystemDevice::new()
        .run_process_streaming_with_encoding(
            scanner().to_owned(),
            scan_arguments(host, port, timeout_seconds),
            timeout_seconds.clamp(1, 30),
            sender,
            ProcessOutputEncoding::Utf8,
        )
        .await
        .map_err(|error| match error {
            DeviceError::Timeout => DeviceError::Ssh {
                backend: "keyscan",
                category: "timeout",
            },
            DeviceError::OutputLimit { .. } => DeviceError::Ssh {
                backend: "keyscan",
                category: "output_limit",
            },
            _ => DeviceError::Ssh {
                backend: "keyscan",
                category: "scan_failed",
            },
        })?;
    parse_scan(host, port, &output.stdout)
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
    persist_scan_locked(scan, path)
}

async fn persist_scan_async(
    scan: &SshHostKeyScan,
    path: &Path,
    deadline: tokio::time::Instant,
) -> Result<PathBuf, DeviceError> {
    validate_scan(scan)?;
    let lock = open_trust_lock(path)?;
    loop {
        if tokio::time::Instant::now() >= deadline {
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
    // 获锁后无 await，也不启动后台写入；取消只能在提交前赢，提交后返回实际结果。
    persist_scan_locked(scan, path)
}

fn persist_scan_locked(scan: &SshHostKeyScan, path: &Path) -> Result<PathBuf, DeviceError> {
    let validated = parse_scan(&scan.host, scan.port, &scan.entries.join("\n"))?;
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(DeviceError::Operation(error.to_string())),
    };
    let target = known_hosts_target(&scan.host, scan.port);
    let mut lines = Vec::new();
    for line in existing.lines() {
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
    let temporary = path.with_extension(format!("{}.tmp", remoteops_domain::RequestId::new()));
    let mut temporary_created = false;
    let result: Result<(), DeviceError> = (|| {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        temporary_created = true;
        file.write_all(format!("{}\n", lines.join("\n")).as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| DeviceError::Operation(error.to_string()))?;
        drop(file);
        #[cfg(windows)]
        super::ssh_atomic_file::replace(&temporary, path)?;
        #[cfg(not(windows))]
        std::fs::rename(&temporary, path)
            .map_err(|_| DeviceError::Operation("SSH 信任库原子替换失败".into()))?;
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
    // 无效确认不触发网络扫描，更不能变成自动首次信任。
    validate_expected(expected_fingerprints)?;
    let overall_deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds.max(1));
    let scan = scan_ssh_host_keys_async(host, port, timeout_seconds).await?;
    verify_expected(&scan, expected_fingerprints)?;
    let deadline =
        overall_deadline.min(tokio::time::Instant::now() + std::time::Duration::from_secs(2));
    persist_scan_async(&scan, &default_ssh_known_hosts_file()?, deadline).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(host: &str, body: &[u8]) -> String {
        let key = [&11_u32.to_be_bytes()[..], b"ssh-ed25519", body].concat();
        format!("{host} ssh-ed25519 {}", BASE64.encode(key))
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
        let task = tokio::spawn(scan_ssh_host_keys_async("127.0.0.1", port, 30));
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
            scan_ssh_host_keys_async("127.0.0.1", port, 1)
                .await
                .is_err()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        holder.abort();
    }
}
