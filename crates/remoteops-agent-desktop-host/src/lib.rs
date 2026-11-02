//! Win32 桌面产品宿主；窗口不持有连接、凭据或权限实现。

use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "agent-runtime")]
mod runtime;

/// 窗口实现类型；仅决定预览产品的数据隔离，不改变核心授权行为。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UiFlavor {
    /// 现有 Win32 原生控件预览。
    #[default]
    Win32,
}

/// 表现层显式提供的宿主选项，不读取另一版本的配置或身份。
#[derive(Clone, Debug)]
pub struct HostOptions {
    /// 诊断演示模式，不启动真实连接。
    pub demo: bool,
    /// 自测模式，不启动真实连接。
    pub self_test: bool,
    /// 产品界面演示，不读取或保存实际配置。
    pub product_demo: bool,
    /// 用户显式指定的本版本配置文件。
    pub agent_config: Option<std::path::PathBuf>,
    /// 窗口实现及对应的数据隔离范围。
    pub ui_flavor: UiFlavor,
    /// 实际宿主程序版本，而非共享库版本。
    pub host_version: String,
    /// 可选的定制产品身份；不由现场配置或环境变量改变。
    #[cfg(feature = "agent-runtime")]
    pub product_profile: Option<remoteops_product::ProductProfile>,
    /// 产品提供的受信任工具资源；普通配置和环境变量不能替换。
    #[cfg(feature = "agent-runtime")]
    pub bundled_tools: Option<remoteops_agent::BundledTools>,
}
impl Default for HostOptions {
    fn default() -> Self {
        Self {
            demo: false,
            self_test: false,
            product_demo: false,
            agent_config: None,
            ui_flavor: UiFlavor::Win32,
            host_version: env!("CARGO_PKG_VERSION").to_owned(),
            #[cfg(feature = "agent-runtime")]
            product_profile: None,
            #[cfg(feature = "agent-runtime")]
            bundled_tools: None,
        }
    }
}

impl UiFlavor {
    /// 返回对应原生宿主的独立缺省身份；CLI 与窗口共用。
    #[cfg(feature = "agent-runtime")]
    #[must_use]
    pub fn profile(self, version: &str) -> remoteops_product::ProductProfile {
        let (id, directory, config, state, prefix) = match self {
            Self::Win32 => (
                "remoteops-agent-native",
                "RemoteOpsNativePreview",
                "native-agent-config.json",
                "native-agent-state.json",
                "REMOTEOPS_NATIVE",
            ),
        };
        remoteops_product::ProductProfile {
            id: id.into(),
            name: "RemoteOps".into(),
            version: version.into(),
            directory_name: directory.into(),
            config_file_name: config.into(),
            state_file_name: state.into(),
            env_prefix: prefix.into(),
            legacy_remoteops_paths: false,
            ..remoteops_product::ProductProfile::default()
        }
    }
    #[cfg(feature = "agent-runtime")]
    fn preferences_filename(self) -> &'static str {
        match self {
            Self::Win32 => "native-gui-preferences.json",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
/// 原生窗口的语言偏好。
pub enum NativeLanguage {
    #[default]
    Chinese,
    English,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
/// 原生窗口的外观偏好。
pub enum NativeAppearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 产品连接阶段；执行权限仍由公共 Agent 核心判定。
pub enum ProductPhase {
    #[default]
    Setup,
    Connecting,
    Waiting,
    Controlled,
    Reconnecting,
    Failed,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只读日志的显示级别，不据此推断任务终态。
pub enum LogLevel {
    Running,
    Success,
    Error,
    Info,
}

/// 真实操作的结构化显示分类，不根据命令或输出文本推断。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ObservationCategory {
    /// Windows CMD 命令。
    Cmd,
    /// Windows PowerShell 或跨平台 PowerShell。
    PowerShell,
    /// 其他系统 Shell，或尚未确认具体解释器的 Shell。
    Shell,
    /// 经 `RemoteOps` 发起的 SSH 操作。
    Ssh,
    /// 串口查询、读写和资源操作。
    Serial,
    /// 尚无结构化来源的记录或其他操作。
    Other,
}

/// 一个已经真实开始、尚未收到终态的请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveOperation {
    /// 完整请求标识，用于并发关联，不包含命令或认证载荷。
    pub request: String,
    /// 来源事件确认的分类。
    pub category: ObservationCategory,
}

#[derive(Clone, Debug)]
/// 一条已脱敏并关联任务的本地显示记录。
pub struct ProductLog {
    /// 记录发生时间，仅作显示。
    pub occurred: String,
    /// 任务关联短号。
    pub request: String,
    /// 操作类型及调用来源。
    pub source: String,
    /// 显示级别。
    pub level: LogLevel,
    /// 已脱敏的展示正文。
    pub message: String,
}

#[derive(Clone, Debug)]
/// 发送 Agent 凭据之前显示的证书确认信息。
pub struct TrustPrompt {
    /// 目标 Relay 地址。
    pub relay: String,
    /// 等待确认的证书 SHA-256 指纹。
    pub fingerprint: String,
    /// 需要确认的原因。
    pub reason: String,
    /// 操作或保存失败时的简短信息。
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // 只读控件快照包含多个独立状态，授权决策仍在核心。
/// 窗口只读快照，包含实际连接状态和有界显示历史。
pub struct ProductSnapshot {
    /// 呈现状态修订号。
    pub revision: u64,
    /// 本地化窗口标题。
    pub title: String,
    /// 调用产品的展示名称，不代替产品身份或权限。
    pub product_name: String,
    /// 当前原生宿主版本。
    pub version: String,
    /// 当前连接阶段。
    pub phase: ProductPhase,
    /// 本地化连接状态。
    pub status: String,
    /// 可供现场排查的脱敏详情。
    pub detail: String,
    /// 是否需要首次设置或修复配置。
    pub setup_needed: bool,
    /// 目标 Relay 地址。
    pub relay: String,
    /// 用于 TLS 校验的服务名称。
    pub server_name: String,
    /// 产品策略是否允许编辑目标地址。
    pub endpoint_editable: bool,
    /// 实际配置文件位置。
    pub config_path: String,
    /// 当前文件交换目录。
    pub transfer_root: String,
    /// 当前进程是否具有提升后的管理员权限。
    pub elevated: bool,
    /// 公共核心报告的能力及可用状态。
    pub capabilities: Vec<(String, bool)>,
    /// 可显示的分组控制码。
    pub code: Option<String>,
    /// 控制码的 UTC 到期秒数。
    pub code_expires_unix: Option<i64>,
    /// 控制码到期倒计时。
    pub countdown: String,
    /// 控制码尚在有效期且可复制。
    pub can_copy: bool,
    /// 当前绑定的控制端数量。
    pub controllers: usize,
    /// 按真实执行状态统计的在途任务数。
    pub running_count: usize,
    /// 已开始且未终止的结构化请求集合，与日志历史容量独立。
    pub active_operations: Vec<ActiveOperation>,
    /// 主窗口活动摘要。
    pub activity: String,
    /// 最近任务的真实终态。
    pub last_result: String,
    /// Relay 权限快照对应的本地化状态。
    pub control_summary: String,
    /// 已确认授权的来源。
    pub control_source: String,
    /// 当前是否允许发起控制模式变更。
    pub control_enabled: bool,
    /// 是否正在等待权限确认。
    pub control_pending: bool,
    /// 已成功保存的新连接默认权限。
    pub default_full: bool,
    /// 权限请求失败或超时提示。
    pub control_error: Option<String>,
    /// 待现场确认的证书，不包含凭据。
    pub tls: Option<TrustPrompt>,
    /// 当前界面语言。
    pub language: NativeLanguage,
    /// 当前界面外观。
    pub appearance: NativeAppearance,
    /// 只在内存保留的有界日志历史。
    pub logs: VecDeque<ProductLog>,
    /// 未读记录数。
    pub unread: usize,
    /// 本地观察订阅中因展示落后而淘汰的日志数，不代表远程传输丢行。
    pub dropped_observations: u64,
}

impl Default for ProductSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            title: format!("RemoteOps · 远程协助 v{}", env!("CARGO_PKG_VERSION")),
            product_name: "RemoteOps".into(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            phase: ProductPhase::Setup,
            status: "首次使用，请配置连接".to_owned(),
            detail: String::new(),
            setup_needed: true,
            relay: String::new(),
            server_name: String::new(),
            endpoint_editable: true,
            config_path: String::new(),
            transfer_root: String::new(),
            elevated: false,
            capabilities: Vec::new(),
            code: None,
            code_expires_unix: None,
            countdown: String::new(),
            can_copy: false,
            controllers: 0,
            running_count: 0,
            active_operations: Vec::new(),
            activity: "系统就绪 · 等待操作".to_owned(),
            last_result: String::new(),
            control_summary: "等待控制端连接".to_owned(),
            control_source: String::new(),
            control_enabled: false,
            control_pending: false,
            default_full: false,
            control_error: None,
            tls: None,
            language: NativeLanguage::default(),
            appearance: NativeAppearance::default(),
            logs: VecDeque::new(),
            unread: 0,
            dropped_observations: 0,
        }
    }
}

impl ProductSnapshot {
    /// 按当前语言选择用于显示的文本。
    #[must_use]
    pub fn t<'a>(&self, chinese: &'a str, english: &'a str) -> &'a str {
        if self.language == NativeLanguage::English {
            english
        } else {
            chinese
        }
    }

    /// 将已脱敏日志保存到有界显示历史。
    pub fn record(&mut self, log: ProductLog) {
        if self.logs.len() >= 1000 {
            self.logs.pop_front();
        }
        self.logs.push_back(log);
        self.unread = self.unread.saturating_add(1);
        self.revision = self.revision.wrapping_add(1);
    }

    fn update_clock(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let remaining = self.code_expires_unix.map(|end| end.saturating_sub(now));
        self.can_copy = self.code.is_some() && remaining.is_none_or(|seconds| seconds > 0);
        self.countdown = match remaining {
            Some(seconds) if seconds > 0 => format!(
                "{:02}:{:02} {}",
                seconds / 60,
                seconds % 60,
                self.t("后失效", "remaining")
            ),
            Some(_) => self.t("控制码已失效", "Code expired").to_owned(),
            None => self
                .t("等待获取控制码", "Waiting for pairing code")
                .to_owned(),
        };
    }
}

#[derive(Clone, Debug)]
/// 由窗口提交的明确动作；保存和权限变更交给宿主处理。
pub enum ProductAction {
    SaveSetup { relay: String },
    RequestControl { full: bool },
    SaveDefaultControl { full: bool },
    TrustCancel,
    TrustOnce,
    TrustSave,
    SetLanguage(NativeLanguage),
    SetAppearance(NativeAppearance),
    ClearLogs,
    MarkLogsRead,
    Stop,
}

/// 只读终端的语义颜色角色，不根据输出中的关键字猜测。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ObservationRole {
    /// 实际执行命令。
    Command,
    /// 普通返回内容。
    #[default]
    Output,
    /// 已确认的成功结果。
    Success,
    /// 标准错误或失败信息，不代表一定终止。
    Error,
    /// 简短过程说明。
    Info,
}

/// 已脱敏的观察记录；`new_task` 仅在新任务真正开始时恢复自动弹窗。
pub struct Observation {
    /// 已脱敏的终端展示文本。
    pub text: String,
    /// 此文本的结构化显示角色。
    pub role: ObservationRole,
    /// 本条是否标识一个实际开始的新任务。
    pub new_task: bool,
    /// 已验证的操作来源，不从展示文本解析。
    pub category: ObservationCategory,
    /// 完整请求标识；无任务关联的系统记录为空。
    pub request: Option<String>,
}

/// 隔离窗口与公共 Agent 运行时的产品宿主。
pub struct ProductHost {
    state: ProductSnapshot,
    demo: bool,
    // 显式产品演示只投递一次合成观察数据，不创建真实操作或自动打开窗口。
    demo_observations_pending: bool,
    #[cfg(feature = "agent-runtime")]
    runtime: Option<runtime::RuntimeHost>,
}

impl ProductHost {
    /// 创建宿主；界面演示和诊断模式不连接或读取日常配置。
    #[must_use]
    pub fn new(options: &HostOptions) -> Self {
        let mut host = Self {
            state: ProductSnapshot {
                title: format!("RemoteOps · 远程协助 v{}", options.host_version),
                version: options.host_version.clone(),
                ..ProductSnapshot::default()
            },
            demo: options.product_demo,
            demo_observations_pending: options.product_demo,
            #[cfg(feature = "agent-runtime")]
            runtime: None,
        };
        #[cfg(feature = "agent-runtime")]
        if let Some(profile) = &options.product_profile {
            host.state.product_name.clone_from(&profile.name);
            host.state.version.clone_from(&profile.version);
            host.state.title = format!("{} · 远程协助 v{}", profile.name, profile.version);
        }
        if options.product_demo {
            host.state.phase = ProductPhase::Waiting;
            host.state.setup_needed = false;
            "等待工程师连接 · 界面演示".clone_into(&mut host.state.status);
            "demo.invalid:7443".clone_into(&mut host.state.relay);
            host.state.code = Some("123-456-789".to_owned());
            host.state.code_expires_unix = Some(
                i64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                )
                .unwrap_or(i64::MAX)
                .saturating_add(600),
            );
            host.state.capabilities = ["Shell", "SSH", "串口", "文件传输"]
                .map(|s| (s.to_owned(), true))
                .to_vec();
            "界面演示，不读取或保存实际配置".clone_into(&mut host.state.config_path);
            for (level, label) in [
                (LogLevel::Running, "Running"),
                (LogLevel::Success, "Success"),
                (LogLevel::Error, "Error"),
                (LogLevel::Info, "Info"),
            ] {
                host.state.record(ProductLog {
                    occurred: "12:00:00".into(),
                    request: "demo".into(),
                    source: "演示".into(),
                    level,
                    message: format!("[演示] {label} · 仅用于界面筛选验收，未执行任何命令"),
                });
            }
        } else if !options.demo && !options.self_test {
            #[cfg(feature = "agent-runtime")]
            match runtime::RuntimeHost::load(options, &mut host.state) {
                Ok(runtime) => host.runtime = Some(runtime),
                Err(error) => host.state.detail = error,
            }
            #[cfg(not(feature = "agent-runtime"))]
            {
                "仅界面构建不包含连接能力".clone_into(&mut host.state.detail);
            }
        }
        if host.demo {
            host.refresh_demo_language();
        }
        host.state.update_clock();
        host
    }

    /// 记录当前进程实际权限，供只读连接详情展示。
    pub fn set_elevated(&mut self, elevated: bool) {
        self.state.elevated = elevated;
    }

    /// 取得当前只读快照，倒计时以实际系统时间更新。
    #[must_use]
    pub fn snapshot(&self) -> ProductSnapshot {
        let mut result = self.state.clone();
        result.update_clock();
        result
    }

    /// 排空有界后台事件并返回已经脱敏的操作观察记录。
    // 无核心构建保留相同宿主接口，窗口无需按构建模式分支。
    #[cfg_attr(not(feature = "agent-runtime"), allow(clippy::unused_self))]
    pub fn tick(&mut self) -> Vec<Observation> {
        #[cfg(feature = "agent-runtime")]
        if let Some(runtime) = self.runtime.as_mut() {
            return runtime.tick(&mut self.state);
        }
        if self.demo
            && std::mem::take(&mut self.demo_observations_pending)
            && self.state.phase != ProductPhase::Stopped
        {
            return demo_observations();
        }
        Vec::new()
    }

    /// 提交用户动作，保存失败或权限未确认时保留错误状态。
    #[cfg_attr(not(feature = "agent-runtime"), allow(clippy::needless_pass_by_value))]
    ///
    /// # Errors
    /// 配置保存、权限确认或宿主生命周期不能继续时返回已脱敏原因。
    pub fn action(&mut self, action: ProductAction) -> Result<(), String> {
        match &action {
            ProductAction::ClearLogs => {
                self.state.logs.clear();
                self.state.unread = 0;
            }
            ProductAction::MarkLogsRead => self.state.unread = 0,
            ProductAction::SetLanguage(language) => self.state.language = *language,
            ProductAction::SetAppearance(appearance) => self.state.appearance = *appearance,
            _ => {}
        }
        self.state.revision = self.state.revision.wrapping_add(1);
        if self.demo {
            if matches!(action, ProductAction::SetLanguage(_)) {
                self.refresh_demo_language();
            }
            match action {
                ProductAction::Stop => {
                    self.state.phase = ProductPhase::Stopped;
                    "已停止 · 界面演示".clone_into(&mut self.state.status);
                }
                ProductAction::RequestControl { .. } | ProductAction::SaveDefaultControl { .. } => {
                    return Err("界面演示未连接控制端".to_owned());
                }
                _ => {}
            }
            return Ok(());
        }
        #[cfg(feature = "agent-runtime")]
        if let Some(runtime) = self.runtime.as_mut() {
            return runtime.action(action, &mut self.state);
        }
        match action {
            ProductAction::SetLanguage(_)
            | ProductAction::SetAppearance(_)
            | ProductAction::ClearLogs
            | ProductAction::MarkLogsRead
            | ProductAction::Stop => Ok(()),
            _ => Err("当前构建或配置不能建立连接".to_owned()),
        }
    }

    fn refresh_demo_language(&mut self) {
        self.state.title = format!(
            "{} · {} v{}",
            self.state.product_name,
            self.state.t("界面演示", "UI demonstration"),
            self.state.version
        );
        self.state.status = self
            .state
            .t(
                "等待工程师连接 · 界面演示",
                "Waiting for engineer · UI demonstration",
            )
            .to_owned();
        self.state.activity = self
            .state
            .t("系统就绪 · 等待操作", "Ready · waiting for operations")
            .to_owned();
        self.state.config_path = self
            .state
            .t(
                "界面演示，不读取或保存实际配置",
                "UI demonstration; no real configuration is loaded or saved",
            )
            .to_owned();
        self.state.capabilities = [
            "Shell",
            "SSH",
            self.state.t("串口", "Serial"),
            self.state.t("文件传输", "File transfer"),
        ]
        .map(|text| (text.to_owned(), true))
        .to_vec();
    }

    /// 请求后台停止并有界等待，不让窗口析构无限阻塞。
    #[cfg_attr(not(feature = "agent-runtime"), allow(clippy::unused_self))]
    pub fn shutdown(&mut self) -> bool {
        #[cfg(feature = "agent-runtime")]
        if let Some(runtime) = self.runtime.as_mut() {
            return runtime.shutdown();
        }
        true
    }
}

// 所有行显式标明合成来源；颜色由结构化角色给出，不根据演示词语猜测。
fn demo_observations() -> Vec<Observation> {
    use ObservationCategory::{Cmd, PowerShell};
    use ObservationRole::{Command, Error, Output, Success};
    [
        (Cmd, Command, "CMD > echo 中文终端示例"),
        (Cmd, Output, "中文返回正常 · UTF-8"),
        (Cmd, Output, r#"{"error":"ordinary data, not a failure"}"#),
        (Cmd, Success, "执行完成 · 退出码 0"),
        (PowerShell, Command, "PS > Get-Example"),
        (PowerShell, Error, "示例失败 · 退出码 1"),
    ]
    .into_iter()
    .map(|(category, role, text)| Observation {
        text: format!("[界面演示/合成数据，未实际执行] {text}"),
        role,
        new_task: false,
        category,
        request: Some(
            if category == Cmd {
                "demo-cmd"
            } else {
                "demo-powershell"
            }
            .into(),
        ),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_host_keeps_frontend_version_and_demo_actions() {
        let mut host = ProductHost::new(&HostOptions {
            product_demo: true,
            ui_flavor: UiFlavor::Win32,
            host_version: "9.8.7-test".into(),
            ..HostOptions::default()
        });
        assert_eq!(host.snapshot().version, "9.8.7-test");
        assert!(host.snapshot().title.ends_with("v9.8.7-test"));
        host.action(ProductAction::SetLanguage(NativeLanguage::English))
            .unwrap();
        assert!(
            host.snapshot()
                .title
                .contains("UI demonstration v9.8.7-test")
        );
        assert!(host.snapshot().unread > 0);
        host.action(ProductAction::MarkLogsRead).unwrap();
        assert_eq!(host.snapshot().unread, 0);
        host.action(ProductAction::ClearLogs).unwrap();
        assert!(host.snapshot().logs.is_empty());
        host.action(ProductAction::Stop).unwrap();
        assert_eq!(host.snapshot().phase, ProductPhase::Stopped);
        assert!(host.tick().is_empty());
        assert!(host.shutdown());
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn win32_profile_keeps_preview_identity_config_and_preferences() {
        let profile = UiFlavor::Win32.profile("0.3.0");
        assert!(profile.validate().is_ok());
        assert_eq!(profile.directory_name, "RemoteOpsNativePreview");
        assert_eq!(profile.config_file_name, "native-agent-config.json");
        assert_eq!(profile.state_file_name, "native-agent-state.json");
        assert_eq!(profile.env_prefix, "REMOTEOPS_NATIVE");
        assert!(!profile.legacy_remoteops_paths);
        assert_eq!(
            UiFlavor::Win32.preferences_filename(),
            "native-gui-preferences.json"
        );
        assert_eq!(profile.version, "0.3.0");
    }

    #[test]
    fn expired_code_cannot_be_copied_and_history_is_bounded() {
        let mut snapshot = ProductSnapshot {
            code: Some("123456789".into()),
            code_expires_unix: Some(1),
            ..ProductSnapshot::default()
        };
        snapshot.update_clock();
        assert!(!snapshot.can_copy);
        for index in 0..1200 {
            snapshot.record(ProductLog {
                occurred: String::new(),
                request: String::new(),
                source: "Shell".into(),
                level: LogLevel::Info,
                message: index.to_string(),
            });
        }
        assert_eq!(snapshot.logs.len(), 1000);
        assert_eq!(snapshot.logs[0].message, "200");
    }

    #[test]
    fn clearing_logs_preserves_structured_activity_in_runtime_independent_snapshot() {
        let mut host = ProductHost::new(&HostOptions {
            product_demo: true,
            ..HostOptions::default()
        });
        let active = ActiveOperation {
            request: "fixture-request".into(),
            category: ObservationCategory::Cmd,
        };
        host.state.active_operations.push(active.clone());
        host.state.running_count = 1;
        host.action(ProductAction::ClearLogs).unwrap();
        let snapshot = host.snapshot();
        assert!(snapshot.logs.is_empty());
        assert_eq!(snapshot.running_count, 1);
        assert_eq!(snapshot.active_operations, vec![active]);
    }

    #[test]
    fn product_demo_has_once_only_semantic_samples_without_real_activity_or_log_changes() {
        let path = std::env::temp_dir().join(format!(
            "remoteops-demo-unused-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!path.exists());
        let mut host = ProductHost::new(&HostOptions {
            product_demo: true,
            agent_config: Some(path.clone()),
            ..HostOptions::default()
        });
        let before = host.snapshot().logs.len();
        let rows = host.tick();
        assert_eq!(
            rows.iter().map(|row| row.role).collect::<Vec<_>>(),
            vec![
                ObservationRole::Command,
                ObservationRole::Output,
                ObservationRole::Output,
                ObservationRole::Success,
                ObservationRole::Command,
                ObservationRole::Error,
            ]
        );
        assert!(
            rows.iter()
                .all(|row| !row.new_task && row.text.contains("界面演示/合成数据，未实际执行"))
        );
        assert!(rows.iter().any(|row| row.text.contains("中文返回正常")));
        assert!(
            rows.iter()
                .any(|row| row.role == ObservationRole::Output && row.text.contains(r#""error""#))
        );
        assert!(
            rows.iter()
                .any(|row| row.category == ObservationCategory::Cmd)
        );
        assert!(
            rows.iter()
                .any(|row| row.category == ObservationCategory::PowerShell)
        );
        assert!(rows.iter().all(|row| {
            row.request
                .as_deref()
                .is_some_and(|value| value.starts_with("demo-"))
        }));
        assert!(host.tick().is_empty());
        let snapshot = host.snapshot();
        assert_eq!(snapshot.running_count, 0);
        assert!(snapshot.active_operations.is_empty());
        assert_eq!(snapshot.logs.len(), before);
        assert!(!path.exists());
        #[cfg(feature = "agent-runtime")]
        assert!(host.runtime.is_none());
    }

    #[test]
    fn non_product_diagnostics_and_stopped_demo_do_not_receive_semantic_samples() {
        let mut host = ProductHost::new(&HostOptions {
            self_test: true,
            ..HostOptions::default()
        });
        assert!(host.tick().is_empty());
        let mut demo = ProductHost::new(&HostOptions {
            product_demo: true,
            ..HostOptions::default()
        });
        demo.action(ProductAction::Stop).unwrap();
        assert!(demo.tick().is_empty());
        assert!(demo.tick().is_empty());
    }

    #[test]
    fn product_demo_never_opens_runtime_or_writes_configuration() {
        let mut host = ProductHost::new(&HostOptions {
            product_demo: true,
            ..HostOptions::default()
        });
        assert!(host.snapshot().can_copy);
        assert!(!host.tick().is_empty());
        assert!(host.tick().is_empty());
        #[cfg(feature = "agent-runtime")]
        assert!(host.runtime.is_none());
        assert!(
            host.action(ProductAction::RequestControl { full: true })
                .is_err()
        );
        assert!(host.shutdown());
    }
}
