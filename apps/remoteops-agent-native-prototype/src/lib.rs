//! 可复用的 Win32 界面入口；产品身份由调用方提供。

#[cfg(windows)]
mod activity_view;
#[cfg(windows)]
mod backend;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod model;
#[cfg(windows)]
#[cfg_attr(not(feature = "agent-runtime"), allow(dead_code))] // UI-only 保留同一呈现事件契约。
mod product;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod render_chunk;
#[cfg(windows)]
#[allow(unsafe_code)] // Win32 表现层复用共享宿主，原生调用集中于窗口边界。
mod style_preview;
#[cfg(windows)]
#[allow(unsafe_code)]
mod win32;

#[cfg(feature = "agent-runtime")]
mod console;
#[cfg(feature = "agent-runtime")]
pub use console::resolve_config;

use serde::Serialize;
use std::{path::PathBuf, process::ExitCode};

/// 仅小样自身使用的启动选项，不读取日常产品配置或环境凭据。
#[derive(Default)]
#[cfg_attr(not(windows), allow(dead_code))] // 非 Windows 仅保留参数校验与模型测试。
#[allow(clippy::struct_excessive_bools)] // 独立命令行开关保留既有诊断模式兼容性。
struct Options {
    self_test: bool,
    // 原有诊断按钮仅在显式诊断演示中保留。
    demo: bool,
    // 产品布局演示不启动网络或读写日常配置。
    product_demo: bool,
    // 独立样式预览只展示合成状态，由用户确认外观后再接入业务。
    style_demo: bool,
    visible: bool,
    report: Option<PathBuf>,
    stress_lines: u64,
    exit_after_ms: Option<u64>,
    preview_dpi: Option<u32>,
    agent_config: Option<PathBuf>,
    cli: bool,
    // 许可只在用户打开页面时展示，不随业务快照重复克隆。
    legal_notices: Option<&'static str>,
    #[cfg(feature = "agent-runtime")]
    bundled_tools: Option<remoteops_agent::BundledTools>,
    #[cfg(feature = "agent-runtime")]
    product_profile: Option<remoteops_product::ProductProfile>,
}

impl Options {
    fn parse() -> Result<Self, String> {
        Self::parse_from(std::env::args_os().skip(1))
    }

    fn parse_from(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut result = Self {
            stress_lines: 100_000,
            style_demo: cfg!(feature = "style-preview"),
            ..Self::default()
        };
        while let Some(arg) = args.next() {
            let flag = arg.to_string_lossy();
            match flag.as_ref() {
                "--self-test" => result.self_test = true,
                "--visible" => result.visible = true,
                "--demo" => result.demo = true,
                "--product-demo" => result.product_demo = true,
                "--style-demo" => result.style_demo = true,
                "--report" => result.report = Some(args.next().ok_or("缺少报告路径")?.into()),
                "--cli" => result.cli = true,
                "--config" | "--agent-config" => {
                    result.agent_config = Some(args.next().ok_or("缺少显式配置路径")?.into());
                }
                "--stress-lines" | "--exit-after-ms" | "--preview-dpi" => {
                    let value = args
                        .next()
                        .ok_or("缺少参数值")?
                        .to_string_lossy()
                        .parse::<u64>()
                        .map_err(|_| "参数必须是非负整数")?;
                    match flag.as_ref() {
                        "--stress-lines" => result.stress_lines = value.clamp(1, 1_000_000),
                        "--exit-after-ms" => result.exit_after_ms = Some(value.clamp(1, 60_000)),
                        _ => {
                            result.preview_dpi =
                                Some(u32::try_from(value).map_err(|_| "DPI 超界")?.clamp(96, 288));
                        }
                    }
                }
                _ => return Err(format!("未知参数：{flag}")),
            }
        }
        if result.style_demo
            && (result.agent_config.is_some()
                || result.self_test
                || result.product_demo
                || result.demo
                || result.report.is_some()
                || result.exit_after_ms.is_some()
                || result.preview_dpi.is_some())
        {
            return Err("样式预览不接受现场配置、业务演示或诊断报告参数".to_owned());
        }
        if (result.self_test || result.product_demo) && result.agent_config.is_some() {
            return Err("自测或产品界面演示不接受远程 Agent 配置".to_owned());
        }
        Ok(result)
    }

    // 定制产品只能经过已注入 Profile 的产品窗口，不能进入旧小样的连接路径。
    #[cfg_attr(not(feature = "agent-runtime"), allow(dead_code))]
    fn validate_custom_product(&self) -> Result<(), &'static str> {
        if self.demo || self.self_test || self.preview_dpi.is_some() || self.style_demo {
            return Err("定制产品不支持旧小样诊断入口");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Check {
    name: String,
    passed: bool,
    detail: String,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    passed: bool,
    checks: Vec<Check>,
    metrics: serde_json::Value,
    environment: serde_json::Value,
    limitations: Vec<String>,
}

// 启动失败也生成固定类别报告，不把原始现场错误写入持久化报告。
fn failure_report() -> Report {
    Report {
        schema_version: 1,
        passed: false,
        checks: vec![Check {
            name: "native_run_completed".into(),
            passed: false,
            detail: "启动或生命周期未完成，未生成正常业务报告".into(),
        }],
        metrics: serde_json::json!({"native_run_failed":true}),
        environment: serde_json::json!({"platform":std::env::consts::OS}),
        limitations: vec!["异常报告不代表业务验收通过".into()],
    }
}
// 普通结束和失败共用落盘路径；相对文件名没有父目录时不创建空路径。
fn write_report(path: &std::path::Path, report: &Report) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(path, bytes)
}
/// 启动独立通用 Win32 候选。
#[must_use]
pub fn run_default() -> ExitCode {
    run_impl(None, ProductOptions::default())
}

/// 定制产品的静态资源；不改变远程配置或能力边界。
#[derive(Default)]
pub struct ProductOptions {
    /// 第三方工具的可信静态资源，实际使用 SSH 时才释放。
    #[cfg(feature = "agent-runtime")]
    pub bundled_tools: Option<remoteops_agent::BundledTools>,
    /// 用户可在关于页查看的内嵌组件许可。
    pub legal_notices: Option<&'static str>,
}

/// 使用产品自身的身份、固定端点与配置启动共享 Win32 界面。
#[cfg(feature = "agent-runtime")]
#[must_use]
pub fn run(profile: remoteops_product::ProductProfile) -> ExitCode {
    run_with_options(profile, ProductOptions::default())
}

/// 使用同一 Win32 产品宿主及由产品提供的可信资源运行。
#[cfg(feature = "agent-runtime")]
#[must_use]
pub fn run_with_options(
    profile: remoteops_product::ProductProfile,
    options: ProductOptions,
) -> ExitCode {
    if let Err(error) = profile.validate() {
        eprintln!("{}", remoteops_agent::redact_diagnostic(&error.to_string()));
        return ExitCode::FAILURE;
    }
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .iter()
        .skip(1)
        .any(|arg| arg == "--help" || arg == "--version")
    {
        return console::run(profile, &args, options.bundled_tools);
    }
    run_impl(Some(profile), options)
}

#[cfg(feature = "agent-runtime")]
type CustomProfile = Option<remoteops_product::ProductProfile>;
#[cfg(not(feature = "agent-runtime"))]
type CustomProfile = Option<()>;

fn run_impl(profile: CustomProfile, product_options: ProductOptions) -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    #[cfg(all(feature = "agent-runtime", not(feature = "style-preview")))]
    if let Some(result) = console::handle_ssh_trust(&args, product_options.bundled_tools.as_ref()) {
        return result;
    }
    #[cfg(feature = "agent-runtime")]
    if args.iter().skip(1).any(|arg| arg == "--cli") {
        return console::run(
            profile.unwrap_or_else(|| {
                remoteops_agent_desktop_host::UiFlavor::Win32.profile(env!("CARGO_PKG_VERSION"))
            }),
            &args,
            product_options.bundled_tools,
        );
    }
    if args.iter().any(|arg| arg == "--help" || arg == "--version") {
        println!(
            "RemoteOps Windows 原生界面小样 {}\n--style-demo 只展示旧版样式、各小窗口和只读观察窗，不连接现场。\n专用 style-preview 构建默认进入样式预览。\n普通构建默认启动独立原生产品；首次使用需要配置 Relay。\n--product-demo 仅演示产品界面；--demo 保留旧诊断按钮。\n--self-test --report PATH [--stress-lines 100000] [--exit-after-ms 100]\n--visible --preview-dpi 144；布局预览不改变系统缩放。",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }
    let options = match Options::parse() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(feature = "agent-runtime")]
    if profile.is_some()
        && let Err(error) = options.validate_custom_product()
    {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let options = Options {
        legal_notices: product_options.legal_notices,
        ..options
    };
    #[cfg(feature = "agent-runtime")]
    let options = Options {
        product_profile: profile,
        bundled_tools: product_options.bundled_tools,
        ..options
    };
    #[cfg(not(feature = "agent-runtime"))]
    let _ = profile;
    if cfg!(feature = "style-preview") || options.style_demo {
        #[cfg(windows)]
        return match style_preview::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("样式预览启动或退出失败：{error}");
                ExitCode::FAILURE
            }
        };
        #[cfg(not(windows))]
        {
            eprintln!("样式预览仅支持 Windows 桌面");
            return ExitCode::FAILURE;
        }
    }
    let report_path = options.report.clone();
    #[cfg(windows)]
    let result = if !options.self_test && !options.demo && options.preview_dpi.is_none() {
        style_preview::run_product(&options)
    } else {
        win32::run(options)
    };
    #[cfg(not(windows))]
    let result: Result<Report, String> = {
        let _ = options;
        Err("此小样仅在 Windows 桌面运行".to_owned())
    };
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            eprintln!("原生界面启动或退出失败：{error}");
            failure_report()
        }
    };
    let passed = report.passed;
    if let Some(path) = report_path
        && let Err(error) = write_report(&path, &report)
    {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod option_tests {
    use super::Options;
    use std::ffi::OsString;

    #[test]
    fn startup_failure_report_is_explicit_and_keeps_no_raw_error() {
        let report = super::failure_report();
        assert!(!report.passed);
        assert_eq!(report.checks[0].name, "native_run_completed");
        assert!(!report.checks[0].passed);
        assert_eq!(report.metrics["native_run_failed"], true);
        assert!(serde_json::to_value(report).is_ok());
    }
    #[test]
    fn failed_report_is_persisted_and_write_failure_is_propagated() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "remoteops-headless-report-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("报告.json");
        let report = super::failure_report();
        super::write_report(&path, &report).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["passed"], false);
        assert_eq!(saved["checks"][0]["name"], "native_run_completed");
        assert!(
            super::write_report(&path.join("cannot-create-under-a-file.json"), &report).is_err()
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }
    #[test]
    fn custom_product_rejects_legacy_diagnostic_routes_before_creating_windows() {
        for args in [
            vec!["--demo", "--config", "other.json"],
            vec!["--self-test"],
            vec!["--preview-dpi", "144"],
            vec!["--style-demo"],
        ] {
            // 专用样式构建可在参数解析阶段更早拒绝这些组合。
            if let Ok(options) = Options::parse_from(args.into_iter().map(OsString::from)) {
                assert!(options.validate_custom_product().is_err());
            }
        }
        assert!(Options::default().validate_custom_product().is_ok());
        assert!(
            Options {
                product_demo: true,
                ..Options::default()
            }
            .validate_custom_product()
            .is_ok()
        );
    }

    #[test]
    fn style_preview_rejects_connection_and_diagnostic_inputs() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(OsString::from));
        assert!(parse(&["--style-demo"]).unwrap().style_demo);
        assert!(parse(&["--style-demo", "--agent-config", "C:/现场配置.json"]).is_err());
        assert!(parse(&["--style-demo", "--self-test"]).is_err());
        assert!(parse(&["--style-demo", "--product-demo"]).is_err());
        assert!(parse(&["--style-demo", "--report", "report.json"]).is_err());
        assert!(parse(&["--style-demo", "--preview-dpi", "144"]).is_err());
    }
}
