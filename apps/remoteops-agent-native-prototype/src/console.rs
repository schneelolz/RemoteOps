//! 在创建任何窗口之前处理产品配置检查和无图形控制台。
use remoteops_product::ProductProfile;
use std::{ffi::OsString, process::ExitCode};

/// 从同一产品身份解析配置；此函数不连接 Relay，也不保存文件。
///
/// # Errors
/// 参数、配置内容或固定端点策略无效时返回错误。
pub fn resolve_config<I, T>(
    profile: &ProductProfile,
    input: I,
) -> anyhow::Result<remoteops_agent::AgentConfig>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    profile.validate()?;
    let options = crate::Options::parse_from(input.into_iter().map(Into::into).skip(1))
        .map_err(anyhow::Error::msg)?;
    if options.style_demo || options.product_demo || options.self_test || options.demo {
        anyhow::bail!("配置检查和控制台不接受演示模式");
    }
    remoteops_agent::AgentConfig::load_file_with_profile(options.agent_config.as_deref(), profile)?
        .resolve_product(profile, None)?
        .normalize_and_validate()
}

pub(super) fn run(
    profile: ProductProfile,
    args: &[OsString],
    bundled_tools: Option<remoteops_agent::BundledTools>,
) -> ExitCode {
    let result = (|| -> anyhow::Result<()> {
        if args.iter().skip(1).any(|arg| arg == "--help") {
            println!(
                "{} {}\n--cli 无图形模式\n--config PATH 指定产品配置\nhelp / status / full / confirm / quit",
                profile.name, profile.version
            );
            return Ok(());
        }
        if args.iter().skip(1).any(|arg| arg == "--version") {
            println!("{} {}", profile.id, profile.version);
            return Ok(());
        }
        let config = resolve_config(&profile, args.iter().cloned())?;
        remoteops_native_host::ensure_console()?;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(remoteops_agent::run_product_console_with_tools(
                profile,
                config,
                bundled_tools,
            ))
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "控制台启动失败：{}",
                remoteops_agent::redact_diagnostic(&format!("{error:#}"))
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "style-preview"))]
pub(super) fn handle_ssh_trust(
    args: &[OsString],
    tools: Option<&remoteops_agent::BundledTools>,
) -> Option<ExitCode> {
    if args
        .get(1)
        .is_some_and(|value| value == "--ssh-key-scan" || value == "--ssh-key-trust")
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let result = runtime
            .map_err(|error| error.to_string())
            .and_then(|runtime| {
                runtime
                    .block_on(remoteops_agent::handle_ssh_trust_cli_with_tools(
                        args, tools,
                    ))
                    .ok_or_else(|| "未识别 SSH 主机密钥命令".to_owned())?
                    .map_err(|error| remoteops_agent::redact_diagnostic(&error.to_string()))
            });
        return Some(match result {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_query_rejects_demo_and_unknown_inputs_without_opening_windows() {
        for flag in ["--product-demo", "--demo", "--self-test", "--unexpected"] {
            assert!(resolve_config(&ProductProfile::default(), ["agent", flag]).is_err());
        }
    }
}
