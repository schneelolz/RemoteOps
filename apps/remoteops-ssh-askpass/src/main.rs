//! OpenSSH askpass 的安全文件交接；环境只保存受 ACL 保护的文件路径。

fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--protocol-version") {
        print!("remoteops-askpass-file-v1");
        return std::process::ExitCode::SUCCESS;
    }
    let Some(path) = std::env::var_os("REMOTEOPS_SSH_PASSWORD_FILE") else {
        return std::process::ExitCode::FAILURE;
    };
    match read_password(std::path::Path::new(&path)) {
        Ok(password) => {
            // stdout 仅由 OpenSSH askpass 协议读取；不进入 RemoteOps 操作日志。
            print!("{password}");
            std::process::ExitCode::SUCCESS
        }
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

fn read_password(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut value = String::new();
    file.take(65_537).read_to_string(&mut value)?;
    if value.len() > 65_536 || value.contains(['\r', '\n', '\0']) {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_protocol_reads_unicode_and_rejects_multiline_or_missing() {
        let path =
            std::env::temp_dir().join(format!("remoteops-askpass-test-{}", std::process::id()));
        std::fs::write(&path, "测试秘密abc").unwrap();
        assert_eq!(read_password(&path).unwrap(), "测试秘密abc");
        std::fs::write(&path, "invalid\nsecret").unwrap();
        assert!(read_password(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(read_password(&path).is_err());
    }
}
