//! 测试密钥仅用于启动，向 Relay 发送标准化 GUID 的摘要。
use std::{ffi::OsString, fs::File, io::Read};

use anyhow::{Context, bail};
use remoteops_audit::sha256_bytes;
use uuid::Uuid;

/// 启动参数请求的测试密钥动作。
#[derive(Default)]
pub enum TestKeyAction {
    /// 使用正常的启动配对行为。
    #[default]
    Normal,
    /// 离线生成新的 GUID 后退出。
    Generate,
    /// 使用本次启动的测试密钥摘要。
    Use(String),
}

/// 生成随机 v4 GUID，不读取配置或连接 Relay。
#[must_use]
pub fn generate_test_key() -> String {
    Uuid::new_v4().to_string()
}

/// 分离测试参数，按相同规则检查 CLI 与各桌面入口的输入。
///
/// # Errors
///
/// 测试参数冲突、缺值，或无法读取和校验 GUID 时返回错误。
pub fn split_test_key_args(args: Vec<OsString>) -> anyhow::Result<(Vec<OsString>, TestKeyAction)> {
    let mut args = args.into_iter();
    let mut remaining = args.next().into_iter().collect::<Vec<_>>();
    let mut action = TestKeyAction::Normal;
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.to_str() {
            Some(flag @ ("--gen-key" | "--key" | "--key-file")) => (flag, None),
            Some(value) if value.starts_with("--key=") => ("--key", Some(&value[6..])),
            Some(value) if value.starts_with("--key-file=") => ("--key-file", Some(&value[11..])),
            _ => {
                remaining.push(arg);
                continue;
            }
        };
        if !matches!(action, TestKeyAction::Normal) {
            bail!("--gen-key、--key 和 --key-file 只能出现一次且不能混用");
        }
        action = if flag == "--gen-key" {
            TestKeyAction::Generate
        } else {
            let value = inline
                .map(OsString::from)
                .or_else(|| args.next())
                .filter(|value| !value.to_string_lossy().starts_with("--"))
                .context("缺少测试密钥参数值")?;
            let input = if flag == "--key-file" {
                let file = File::open(value).context("无法打开测试密钥文件")?;
                let mut text = String::new();
                file.take(4097)
                    .read_to_string(&mut text)
                    .context("无法读取 UTF-8 测试密钥文件")?;
                if text.len() > 4096 {
                    bail!("测试密钥文件只能包含一个 GUID");
                }
                text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
            } else {
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("测试密钥必须是有效 GUID"))?
            };
            TestKeyAction::Use(fingerprint(&input)?)
        };
    }
    Ok((remaining, action))
}

fn fingerprint(input: &str) -> anyhow::Result<String> {
    let value = input.trim();
    if value.len() != 36 {
        bail!("测试密钥必须是标准带连字符的非空 GUID");
    }
    let id = Uuid::try_parse(value)
        .ok()
        .filter(|id| !id.is_nil() && value.eq_ignore_ascii_case(&id.to_string()))
        .context("测试密钥必须是标准带连字符的非空 GUID")?;
    Ok(sha256_bytes(id.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn normalizes_key_sources_and_rejects_invalid_input() {
        let key = generate_test_key();
        let fingerprint = fingerprint(&key).unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "\u{feff} {} ", key.to_uppercase()).unwrap();
        let args = vec![
            "agent".into(),
            "--cli".into(),
            "--key-file".into(),
            file.path().into(),
        ];
        let (remaining, action) = split_test_key_args(args).unwrap();
        assert_eq!(
            remaining,
            [OsString::from("agent"), OsString::from("--cli")]
        );
        assert!(matches!(action, TestKeyAction::Use(value) if value == fingerprint));
        for args in [
            vec!["agent", "--key"],
            vec!["agent", "--key", ""],
            vec!["agent", "--key", "00000000-0000-0000-0000-000000000000"],
            vec!["agent", "--key", "bad"],
            vec!["agent", "--gen-key", "--key", &key],
            vec!["agent", "--key", &key, "--key-file", "ignored"],
        ] {
            assert!(split_test_key_args(args.into_iter().map(Into::into).collect()).is_err());
        }
    }
}
