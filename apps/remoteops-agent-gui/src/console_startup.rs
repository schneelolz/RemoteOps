//! 在任何原生窗口初始化之前启动同一产品的无图形控制台宿主。
use super::{Args, ProductProfile};

/// 仅用于在解析错误和帮助输出之前准备 Windows 控制台，不决定实际运行模式。
pub(super) fn console_requested(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    arguments
        .into_iter()
        .skip(1)
        .take_while(|argument| argument != "--")
        .any(|argument| argument == "--cli")
}

pub(super) fn run(args: &Args, profile: ProductProfile) -> std::process::ExitCode {
    let result = (|| {
        let config = args.merge(&profile)?.normalize_and_validate()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(remoteops_agent::run_product_console(profile, config))
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "控制台模式启动失败：{}",
                remoteops_agent::redact_diagnostic(&format!("{error:#}"))
            );
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_preparation_respects_argument_terminator() {
        for (arguments, expected) in [
            (vec!["agent", "--cli", "--help"], true),
            (vec!["agent", "--", "--cli"], false),
            (vec!["agent", "--renderer", "glow"], false),
        ] {
            assert_eq!(
                console_requested(arguments.into_iter().map(Into::into)),
                expected
            );
        }
    }

    #[test]
    fn console_reuses_product_configuration_and_identity() {
        let directory = tempfile::tempdir().expect("临时配置目录");
        let path = directory.path().join("agent.json");
        std::fs::write(&path, r#"{"relay":"relay.example.test:7443","server_name":"relay.example.test","default_full_control":false}"#).unwrap();
        let profile = ProductProfile {
            id: "console-product-test".into(),
            name: "控制台产品测试".into(),
            directory_name: "ConsoleProductTest".into(),
            env_prefix: "CONSOLE_PRODUCT_TEST".into(),
            legacy_remoteops_paths: false,
            ..ProductProfile::default()
        };
        let base = vec![
            std::ffi::OsString::from("agent"),
            "--config".into(),
            path.into_os_string(),
        ];
        let gui = Args::parse_for(&profile, base.clone())
            .unwrap()
            .merge(&profile)
            .unwrap();
        let mut console_arguments = base;
        console_arguments.push("--cli".into());
        let console = Args::parse_for(&profile, console_arguments).unwrap();
        assert!(console.cli);
        let console = console.merge(&profile).unwrap();
        assert_eq!(console.relay, gui.relay);
        assert_eq!(console.server_name, gui.server_name);
        assert_eq!(console.state_file, gui.state_file);
        assert_eq!(console.transfer_root, gui.transfer_root);
        assert_eq!(console.default_full_control, gui.default_full_control);
    }

    #[test]
    fn console_rejects_visual_demo_instead_of_starting_a_window() {
        assert!(Args::parse_for(&ProductProfile::default(), ["agent", "--cli", "--demo"]).is_err());
    }
}
