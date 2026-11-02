//! Agent 自带的 SSH 主机密钥核对入口；只接受明确目标和已核对指纹。
use std::ffi::OsString;

/// 处理 SSH 主机密钥命令；不匹配时交还正常 Agent 入口。
///
/// # Errors
/// 目标、端口、指纹格式错误，或扫描／核对／保存失败时返回脱敏原因。
pub async fn handle_ssh_trust_cli(args: &[OsString]) -> Option<anyhow::Result<serde_json::Value>> {
    let action = args.get(1)?.to_str()?;
    if !matches!(action, "--ssh-key-scan" | "--ssh-key-trust") {
        return None;
    }
    Some(run(action, &args[2..]).await)
}

async fn run(action: &str, args: &[OsString]) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        if action == "--ssh-key-scan" {
            args.len() == 2
        } else {
            (3..=18).contains(&args.len())
        },
        "用法：--ssh-key-scan HOST PORT；--ssh-key-trust HOST PORT 完整指纹 [完整指纹...]"
    );
    let host = args[0]
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("SSH 目标必须是有效 Unicode"))?;
    let port: u16 = args[1]
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("SSH 端口格式错误"))?
        .parse()?;
    anyhow::ensure!(port != 0, "SSH 端口必须大于零");
    if action == "--ssh-key-scan" {
        let scan = remoteops_device::scan_ssh_host_keys_async(host, port, 15).await?;
        return Ok(serde_json::json!({
            "status":"scanned_unverified", "host":scan.host, "port":scan.port,
            "fingerprints":scan.fingerprints, "trusted":false,
            "instruction":"请通过可信渠道核对全部指纹；扫描成功不代表目标可信，禁止自动确认。"
        }));
    }
    let fingerprints = parse_fingerprints(&args[2..])?;
    anyhow::ensure!(
        !fingerprints.is_empty() && fingerprints.len() <= 16,
        "SSH 指纹数量不合法"
    );
    let path =
        remoteops_device::trust_ssh_host_keys_verified(host, port, &fingerprints, 15).await?;
    Ok(
        serde_json::json!({"status":"trusted", "host":host, "port":port, "known_hosts_file":path, "fingerprints":fingerprints}),
    )
}

fn parse_fingerprints(args: &[OsString]) -> anyhow::Result<Vec<String>> {
    let values = args
        .iter()
        .map(|value| {
            value
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("SSH 指纹必须是有效 Unicode"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(
        values.iter().map(|value| value.len()).sum::<usize>() <= 8192,
        "SSH 指纹参数超过大小限制"
    );
    // 独立参数避免 Windows PowerShell 5.1 破坏 JSON 内嵌引号；兼容结构化调用的旧 JSON 形式。
    if values.len() == 1 && values[0].trim_start().starts_with('[') {
        return Ok(serde_json::from_str(values[0])?);
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_arguments_preserve_spaces_and_accept_structured_json() {
        let pins = vec![
            "ecdsa-sha2-nistp256 SHA256:abc".to_owned(),
            "ssh-rsa SHA256:def".to_owned(),
        ];
        assert_eq!(
            parse_fingerprints(&pins.iter().map(OsString::from).collect::<Vec<_>>()).unwrap(),
            pins
        );
        assert_eq!(
            parse_fingerprints(&[OsString::from(serde_json::to_string(&pins).unwrap())]).unwrap(),
            pins
        );
        assert_eq!(
            parse_fingerprints(&[OsString::from(format!(
                "  {}",
                serde_json::to_string(&pins).unwrap()
            ))])
            .unwrap(),
            pins
        );
    }

    #[tokio::test]
    async fn cli_rejects_invalid_input_and_does_not_consume_normal_startup() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(
            handle_ssh_trust_cli(&args(&["agent", "--agent-config", "normal.json"]))
                .await
                .is_none()
        );
        assert!(
            handle_ssh_trust_cli(&args(&["agent", "--ssh-key-scan", "host", "0"]))
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            handle_ssh_trust_cli(&args(&["agent", "--ssh-key-trust", "host", "22", "[]"]))
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            handle_ssh_trust_cli(&args(&[
                "agent",
                "--ssh-key-trust",
                "host",
                "22",
                "not-json"
            ]))
            .await
            .unwrap()
            .is_err()
        );
    }
}
