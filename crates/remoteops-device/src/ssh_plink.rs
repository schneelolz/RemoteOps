//! 将已固定的 OpenSSH 公钥映射为 Plink 的显式指纹，不依赖 `PuTTY` 注册表信任。

use super::{BASE64, DeviceError, Digest, Sha256, known_hosts_target};
use base64::Engine as _;
use std::path::{Path, PathBuf};

pub(super) fn bundled_executable() -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .join("tools")
        .join("plink.exe");
    path.is_file().then_some(path)
}

pub(super) fn arguments(
    path: &Path,
    host: &str,
    port: u16,
    user: &str,
    command: &str,
) -> Result<Option<Vec<String>>, DeviceError> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| DeviceError::Operation("无法读取 SSH 主机信任文件".to_owned()))?;
    let pins = fingerprints(&text, &known_hosts_target(host, port));
    if pins.is_empty() {
        return Ok(None);
    }
    let mut args = [
        "-ssh",
        "-2",
        "-batch",
        "-noagent",
        "-noshare",
        "-no-trivial-auth",
        "-no-antispoof",
        "-T",
        "-a",
        "-x",
        "-P",
        &port.to_string(),
        "-l",
        user,
    ]
    .map(str::to_owned)
    .to_vec();
    // 不存在的随机会话会重置为编译默认值，隔离 Default Settings 与同名主机会话。
    args.splice(
        0..0,
        [
            "-load".to_owned(),
            format!("RemoteOps-{}", remoteops_domain::RequestId::new()),
        ],
    );
    for pin in pins {
        args.extend(["-hostkey".to_owned(), pin]);
    }
    args.extend([host.to_owned(), command.to_owned()]);
    Ok(Some(args))
}

fn fingerprints(text: &str, target: &str) -> Vec<String> {
    let mut result = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        // 标记、哈希主机名与通配/排除规则留给 OpenSSH，绝不近似解释信任策略。
        if fields.first().is_some_and(|value| value.starts_with('@')) {
            return Vec::new();
        }
        if fields.len() < 3 || !fields[0].split(',').any(|name| name == target) {
            continue;
        }
        if fields[0].contains(['*', '?', '!']) {
            return Vec::new();
        }
        let Ok(key) = BASE64.decode(fields[2]) else {
            return Vec::new();
        };
        let Some(length) = key
            .get(..4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
        else {
            return Vec::new();
        };
        let Ok(length) = usize::try_from(length) else {
            return Vec::new();
        };
        if key.get(4..4usize.saturating_add(length)) != Some(fields[1].as_bytes()) {
            return Vec::new();
        }
        let pin = format!(
            "SHA256:{}",
            BASE64.encode(Sha256::digest(key)).trim_end_matches('=')
        );
        if !result.contains(&pin) {
            result.push(pin);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        BASE64.encode([&7u32.to_be_bytes()[..], b"ssh-rsa", b"test-key"].concat())
    }
    #[test]
    fn exact_hosts_ports_and_key_types_are_required() {
        let k = key();
        let text = format!("example,alias ssh-rsa {k}\n[example]:2222 ssh-rsa {k}");
        assert_eq!(fingerprints(&text, "example").len(), 1);
        assert_eq!(fingerprints(&text, "[example]:2222").len(), 1);
        assert_eq!(fingerprints(&text, "[example]:22"), Vec::<String>::new());
        assert_eq!(
            fingerprints(&format!("example ssh-ed25519 {k}"), "example"),
            Vec::<String>::new()
        );
    }
    #[test]
    fn revocation_and_unsupported_rules_fall_back_without_accepting_any_key() {
        let k = key();
        for text in [
            format!("@revoked example ssh-rsa {k}"),
            format!("*,example ssh-rsa {k}"),
            format!("|1|hash|hash ssh-rsa {k}"),
            "example ssh-rsa invalid".to_owned(),
        ] {
            assert_eq!(fingerprints(&text, "example"), Vec::<String>::new());
        }
    }

    #[test]
    fn command_arguments_do_not_load_a_host_named_session_or_use_password_arguments() {
        let path = std::env::temp_dir().join(format!(
            "remoteops-plink-arguments-{}",
            remoteops_domain::RequestId::new()
        ));
        std::fs::write(&path, format!("example ssh-rsa {}\n", key())).unwrap();
        let args = arguments(&path, "example", 22, "user", "display version; 中文")
            .unwrap()
            .unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(args[0], "-load");
        assert!(args[1].starts_with("RemoteOps-"));
        assert!(args.iter().any(|value| value == "-batch"));
        assert!(args.iter().any(|value| value == "-hostkey"));
        assert!(!args.iter().any(|value| value == "-pw"));
        assert_eq!(args[args.len() - 2], "example");
        assert_eq!(args.last().unwrap(), "display version; 中文");
    }
}
