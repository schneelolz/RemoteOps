use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::{Command, Stdio},
};

/// 原生 UI 线程结束后才决定是否启动单次兼容进程。
#[derive(Clone, Copy)]
pub(super) enum GuiOutcome {
    Finished(std::process::ExitCode),
    RetryWithGlow,
}

/// 普通退出不启动进程；兼容退出只执行一次启动器并传递其退出状态。
pub(super) fn finish_with(
    outcome: GuiOutcome,
    launch: impl FnOnce() -> std::io::Result<std::process::ExitStatus>,
) -> std::io::Result<std::process::ExitCode> {
    match outcome {
        GuiOutcome::Finished(code) => Ok(code),
        GuiOutcome::RetryWithGlow => {
            let status = launch()?;
            let code = status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1);
            Ok(std::process::ExitCode::from(code))
        }
    }
}

/// 仅在默认渲染器尚未创建应用时，允许从 WGPU 初始化失败回退。
pub(super) fn should_fallback(
    automatic: bool,
    renderer: eframe::Renderer,
    app_started: bool,
    error: &eframe::Error,
) -> bool {
    automatic
        && renderer == eframe::Renderer::Wgpu
        && !app_started
        && matches!(error, eframe::Error::Wgpu(_))
}

/// 以同一可执行文件和显式 Glow 参数创建单次回退命令。
pub(super) fn fallback_command(
    executable: &Path,
    arguments: impl IntoIterator<Item = OsString>,
) -> Command {
    let mut command = Command::new(executable);
    command.args(fallback_arguments(arguments));
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // 防止无控制台的 GUI 启动回退时闪现控制台窗口。
        command.creation_flags(0x0800_0000);
    }
    command
}

fn fallback_arguments(arguments: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut arguments: Vec<_> = arguments.into_iter().collect();
    // 保留参数边界，避免将回退开关插入位置参数区域。
    let boundary = arguments
        .iter()
        .position(|argument| argument == OsStr::new("--"))
        .unwrap_or(arguments.len());
    arguments.splice(boundary..boundary, ["--renderer".into(), "glow".into()]);
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_status(code: u8) -> std::process::ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(u32::from(code))
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(i32::from(code) << 8)
        }
    }

    #[test]
    fn completed_gui_never_starts_a_compatibility_process() {
        let code = finish_with(
            GuiOutcome::Finished(std::process::ExitCode::SUCCESS),
            || panic!("已完成的 GUI 不得再次启动"),
        )
        .expect("普通关闭");
        assert_eq!(code, std::process::ExitCode::SUCCESS);
    }

    #[test]
    fn child_success_and_failure_propagate_without_another_attempt() {
        for status in [0, 7] {
            let calls = std::cell::Cell::new(0);
            let code = finish_with(GuiOutcome::RetryWithGlow, || {
                calls.set(calls.get() + 1);
                Ok(process_status(status))
            })
            .expect("子进程正常结束");
            assert_eq!(calls.get(), 1);
            assert_eq!(code, std::process::ExitCode::from(status));
        }
    }

    #[test]
    fn failed_child_launch_is_returned_once() {
        let calls = std::cell::Cell::new(0);
        let error = finish_with(GuiOutcome::RetryWithGlow, || {
            calls.set(calls.get() + 1);
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "blocked",
            ))
        })
        .expect_err("启动失败必须返回错误");
        assert_eq!(calls.get(), 1);
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    fn adapter_error() -> eframe::Error {
        eframe::Error::Wgpu(
            eframe::egui_wgpu::WgpuError::CustomNativeAdapterSelectionError(
                "no DX12 adapter".to_owned(),
            ),
        )
    }

    #[test]
    fn only_automatic_wgpu_failure_before_app_creation_can_fallback() {
        let error = adapter_error();
        assert!(should_fallback(true, eframe::Renderer::Wgpu, false, &error));
        for (automatic, renderer, app_started) in [
            (false, eframe::Renderer::Wgpu, false),
            (false, eframe::Renderer::Glow, false),
            (true, eframe::Renderer::Glow, false),
            (true, eframe::Renderer::Wgpu, true),
        ] {
            assert!(!should_fallback(automatic, renderer, app_started, &error));
        }
    }

    #[test]
    fn application_creation_failure_is_not_a_renderer_failure() {
        let error = eframe::Error::AppCreation(Box::new(std::io::Error::other("app failure")));
        assert!(!should_fallback(
            true,
            eframe::Renderer::Wgpu,
            false,
            &error
        ));
    }

    #[cfg(windows)]
    #[test]
    fn event_loop_failure_cannot_trigger_another_process() {
        let error = eframe::Error::WinitEventLoop(winit::error::EventLoopError::RecreationAttempt);
        assert!(!should_fallback(
            true,
            eframe::Renderer::Wgpu,
            false,
            &error
        ));
    }

    #[test]
    fn fallback_keeps_configuration_and_literal_argument_values() {
        let arguments = [
            "--config",
            r#"C:\客户配置\带 空格\"quoted".json"#,
            "--relay-url",
            "https://relay.example.test/path?x=1&y=2",
        ]
        .map(OsString::from);
        let actual = fallback_arguments(arguments.clone());
        let mut expected = arguments.to_vec();
        expected.extend(["--renderer".into(), "glow".into()]);
        assert_eq!(actual, expected);
    }

    #[test]
    fn fallback_option_precedes_the_first_argument_terminator() {
        let actual = fallback_arguments(
            ["--config", "agent.json", "--", "--renderer", "wgpu", "--"].map(OsString::from),
        );
        assert_eq!(
            actual,
            [
                "--config",
                "agent.json",
                "--renderer",
                "glow",
                "--",
                "--renderer",
                "wgpu",
                "--",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn fallback_starts_only_the_same_executable_without_a_shell() {
        let executable = Path::new(r"C:\客户 程序\Agent.exe");
        let command = fallback_command(executable, std::iter::empty());
        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("--renderer"), OsStr::new("glow")]
        );
        // 显式 Glow 会在下一进程禁用自动回退，避免重启循环。
        assert!(!should_fallback(
            false,
            eframe::Renderer::Glow,
            false,
            &adapter_error()
        ));
    }
}
