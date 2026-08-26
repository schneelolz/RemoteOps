#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    backtrace::Backtrace,
    env,
    fmt::Write as _,
    io::Write as _,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use clap::Parser;
use eframe::egui::{
    self, Align, Align2, Color32, FontDefinitions, FontFamily, FontId, Frame, Grid, Layout, Margin,
    RichText, Stroke, TextStyle, Vec2, ViewportCommand,
};
use egui_phosphor::regular as icons;
use remoteops_agent::{
    AgentConfig, AgentControllerBinding, AgentEvent, AgentEventSender, AgentPermissionControl,
    active_agent_config_path, default_agent_config_path, initialize_tracing,
    legacy_agent_config_path, run_agent_with_permission_control,
};
use remoteops_domain::{AgentInstanceId, Capability, CapabilitySet};
use remoteops_i18n::{Language, Translator};
use remoteops_protocol::{
    connect_tls, load_native_client_config, load_pinned_client_config, probe_server_certificate,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// 首次设置页使用的固定窗口内部尺寸。
const SETUP_WINDOW_SIZE: Vec2 = Vec2::new(640.0, 330.0);
/// 日常运行页使用的固定窗口内部尺寸。
const RUNNING_WINDOW_SIZE: Vec2 = Vec2::new(520.0, 410.0);
/// Windows 后台进程创建标志，避免权限探测弹出控制台窗口。
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 控制码及复制按钮所在行的固定高度。
const PAIRING_CODE_ROW_HEIGHT: f32 = 40.0;
/// 复制控制码图标按钮的固定边长。
const COPY_CODE_BUTTON_SIZE: f32 = 36.0;

/// 根据当前内容宽度返回控制码行的有界尺寸。
fn pairing_code_row_size(available_width: f32) -> Vec2 {
    Vec2::new(available_width, PAIRING_CODE_ROW_HEIGHT)
}

/// 计算控制码与复制按钮作为整体水平居中时的左侧留白。
fn pairing_code_left_padding(available_width: f32, code_width: f32, item_spacing: f32) -> f32 {
    let content_width = code_width + item_spacing + COPY_CODE_BUTTON_SIZE;
    centered_left_padding(available_width, content_width)
}

/// 计算一组内容在指定宽度内水平居中所需的左侧留白。
fn centered_left_padding(available_width: f32, content_width: f32) -> f32 {
    ((available_width - content_width) / 2.0).max(0.0)
}

/// 固定复制按钮的交互样式，避免悬停或按下触发可见重绘动画。
fn stabilize_copy_button_style(style: &mut egui::Style) {
    style.animation_time = 0.0;
    let inactive = style.visuals.widgets.inactive;
    style.visuals.widgets.hovered = inactive;
    style.visuals.widgets.active = inactive;
}

/// 被控端 GUI 的可选调试参数；正式使用时无需传参。
#[derive(Debug, Parser)]
#[command(version, about = "RemoteOps Windows 现场被控端 GUI")]
struct Args {
    /// Agent JSON 配置文件；默认读取用户本地 `RemoteOps` 目录。
    #[arg(long, env = "REMOTEOPS_AGENT_CONFIG")]
    config: Option<PathBuf>,
    /// Relay TLS 地址。
    #[arg(long, env = "REMOTEOPS_RELAY")]
    relay: Option<String>,
    /// Relay 证书中的 DNS 名称或 IP。
    #[arg(long, env = "REMOTEOPS_SERVER_NAME")]
    server_name: Option<String>,
    /// Relay 自签名 CA 证书；正式公网环境不需要。
    #[arg(long, env = "REMOTEOPS_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// 已由本地用户确认的 Relay 叶证书 SHA-256 指纹。
    #[arg(long, env = "REMOTEOPS_TLS_FINGERPRINT")]
    tls_fingerprint: Option<String>,
    /// 断线后的重试间隔。
    #[arg(long, env = "REMOTEOPS_RETRY_SECONDS")]
    retry_seconds: Option<u64>,
    /// 文件交换目录。
    #[arg(long, env = "REMOTEOPS_TRANSFER_ROOT")]
    transfer_root: Option<PathBuf>,
    /// Agent 身份和恢复令牌状态文件。
    #[arg(long, env = "REMOTEOPS_AGENT_STATE_FILE")]
    state_file: Option<PathBuf>,
    /// 使用虚构状态启动，仅用于视觉验收。
    #[arg(long)]
    demo: bool,
    /// 界面语言；优先级低于已保存的 GUI 设置。
    #[arg(long, env = "REMOTEOPS_LANG")]
    lang: Option<Language>,
    /// 图形渲染器；Windows 默认使用 DirectX 12，glow 仅用于兼容性诊断。
    #[arg(long, env = "REMOTEOPS_RENDERER", default_value_t = default_renderer())]
    renderer: eframe::Renderer,
}

/// 被控端 GUI 保存的非敏感界面设置。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct AgentGuiSettings {
    /// 用户选择的界面语言。
    language: Option<Language>,
}

impl Args {
    /// 把配置文件和可选参数覆盖项合并为首次启动状态。
    fn into_startup(self) -> StartupState {
        let demo = self.demo;
        let explicit_config = self.config.is_some();
        let config_path = self.config.clone().unwrap_or_else(active_agent_config_path);
        let load_result = AgentConfig::load_file(self.config.as_deref());
        let mut config = match load_result {
            Ok(config) => config,
            Err(error) => {
                return StartupState::Setup(AgentSetupForm::new(
                    AgentConfig::default(),
                    config_path,
                    explicit_config,
                    Some(error.to_string()),
                ));
            }
        };
        if let Some(relay) = self.relay {
            config.relay = relay;
        }
        if let Some(server_name) = self.server_name {
            config.server_name = server_name;
        }
        if let Some(ca_cert) = self.ca_cert {
            config.ca_cert = Some(ca_cert);
        }
        if let Some(tls_fingerprint) = self.tls_fingerprint {
            config.tls_fingerprint = Some(tls_fingerprint);
        }
        if let Some(retry_seconds) = self.retry_seconds {
            config.retry_seconds = retry_seconds;
        }
        if let Some(transfer_root) = self.transfer_root {
            config.transfer_root = transfer_root;
        }
        if let Some(state_file) = self.state_file {
            config.state_file = state_file;
        }
        if demo && config.relay.trim().is_empty() {
            "demo.invalid:7443".clone_into(&mut config.relay);
            "demo.invalid".clone_into(&mut config.server_name);
            return StartupState::Ready {
                config,
                config_path,
                demo,
            };
        }
        if config.relay.trim().is_empty() {
            return StartupState::Setup(AgentSetupForm::new(
                config,
                config_path,
                explicit_config,
                None,
            ));
        }
        match config.clone().normalize_and_validate() {
            Ok(config) => StartupState::Ready {
                config,
                config_path,
                demo,
            },
            Err(error) => StartupState::Setup(AgentSetupForm::new(
                config,
                config_path,
                explicit_config,
                Some(error.to_string()),
            )),
        }
    }
}

/// Agent GUI 首次启动后的配置分支。
enum StartupState {
    /// 已获得有效配置，可以开始连接。
    Ready {
        /// 合并后的 Agent 配置。
        config: AgentConfig,
        /// 保存证书指纹时使用的配置路径。
        config_path: PathBuf,
        /// 是否运行视觉演示。
        demo: bool,
    },
    /// 缺少或无法读取配置，需要本地用户处理。
    Setup(AgentSetupForm),
}

/// Agent GUI 首次配置表单。
struct AgentSetupForm {
    /// 表单保存时复用的默认目录和权限配置。
    base_config: AgentConfig,
    /// 计划写入的配置文件路径。
    config_path: PathBuf,
    /// 是否由用户显式指定配置路径。
    explicit_config: bool,
    /// Relay TLS 地址。
    relay: String,
    /// 当前配置错误。
    error: Option<String>,
}

impl AgentSetupForm {
    fn new(
        base_config: AgentConfig,
        config_path: PathBuf,
        explicit_config: bool,
        error: Option<String>,
    ) -> Self {
        Self {
            relay: base_config.relay.clone(),
            base_config,
            config_path,
            explicit_config,
            error,
        }
    }

    fn build_config(&self) -> anyhow::Result<AgentConfig> {
        let mut config = self.base_config.clone();
        config.relay.clone_from(&self.relay);
        config.normalize_and_validate()
    }
}

/// TLS 预检线程返回给 GUI 的状态。
enum TrustBootstrapEvent {
    /// 当前配置可按既定信任策略启动。
    Ready(AgentConfig),
    /// 服务端证书不在系统信任链内或固定指纹已经变化。
    ConfirmationRequired {
        /// 等待用户确认的 Agent 配置。
        config: AgentConfig,
        /// 当前服务端叶证书 SHA-256 指纹。
        fingerprint: String,
        /// 触发确认的原始 TLS 错误。
        reason: String,
    },
    /// TLS 预检线程无法启动。
    Failed(String),
}

/// GUI 当前等待本地用户确认的证书。
struct CertificateConfirmation {
    /// 等待继续启动的 Agent 配置。
    config: AgentConfig,
    /// 当前服务端叶证书 SHA-256 指纹。
    fingerprint: String,
    /// 触发确认的 TLS 错误。
    reason: String,
    /// 保存失败时展示的错误。
    save_error: Option<String>,
}

/// 本地用户对未知证书的决定。
#[derive(Clone, Copy)]
enum CertificateAction {
    /// 取消本次连接。
    Cancel,
    /// 仅在当前进程中固定该证书。
    ContinueOnce,
    /// 固定证书并写入 Agent 配置。
    TrustAndSave,
}

/// GUI 当前呈现的连接阶段。
#[derive(Clone, Debug)]
enum UiStatus {
    /// 正在初始化本机能力。
    Starting,
    /// 正在连接 Relay。
    Connecting,
    /// 已连接 Relay，等待工程师输入控制码。
    Waiting,
    /// 已有控制端与 Agent 绑定。
    Controlled,
    /// 连接中断后自动恢复。
    Reconnecting(String),
    /// Agent 已停止。
    Stopped,
    /// Agent 无法继续运行。
    Failed(String),
}

impl UiStatus {
    /// 返回主状态文字。
    fn label(&self, translator: &Translator) -> String {
        match self {
            Self::Starting => translator.text("status.starting"),
            Self::Connecting => translator.text("status.connecting"),
            Self::Waiting => translator.text("status.waiting"),
            Self::Controlled => translator.text("status.controlled"),
            Self::Reconnecting(_) => translator.text("status.reconnecting"),
            Self::Stopped => translator.text("status.stopped"),
            Self::Failed(_) => translator.text("status.failed"),
        }
    }

    /// 返回状态辅助说明。
    fn detail(&self) -> Option<&str> {
        match self {
            Self::Reconnecting(message) | Self::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// 返回状态指示色。
    const fn color(&self) -> Color32 {
        match self {
            Self::Waiting | Self::Controlled => Color32::from_rgb(30, 185, 105),
            Self::Starting | Self::Connecting | Self::Reconnecting(_) => {
                Color32::from_rgb(37, 112, 232)
            }
            Self::Stopped => Color32::from_rgb(115, 126, 145),
            Self::Failed(_) => Color32::from_rgb(220, 55, 62),
        }
    }
}

/// 被控端窗口状态。
#[allow(clippy::struct_excessive_bools)]
#[allow(dead_code)]
struct RemoteOpsAgentApp {
    /// 当前进程的运行诊断日志。
    diagnostics: StartupDiagnostics,
    /// 向后台 Agent 发送生命周期事件的通道。
    event_sender: AgentEventSender,
    /// 后台 Agent 发来的生命周期事件。
    events: Receiver<AgentEvent>,
    /// TLS 预检线程发送状态的通道。
    trust_sender: Sender<TrustBootstrapEvent>,
    /// TLS 预检线程返回的状态。
    trust_events: Receiver<TrustBootstrapEvent>,
    /// 通知后台 Agent 停止。
    shutdown: watch::Sender<bool>,
    /// 后台线程句柄；进程退出时由操作系统完成最终回收。
    worker: Option<JoinHandle<()>>,
    /// TLS 预检线程句柄。
    trust_worker: Option<JoinHandle<()>>,
    /// 首次运行配置表单。
    setup: Option<AgentSetupForm>,
    /// 当前 Agent 配置文件路径。
    config_path: PathBuf,
    /// 等待本地用户确认的自签名或变化证书。
    certificate_confirmation: Option<CertificateConfirmation>,
    /// 当前连接状态。
    status: UiStatus,
    /// 当前 Agent 标识。
    agent_instance_id: Option<AgentInstanceId>,
    /// 当前 Relay 地址。
    relay: String,
    /// 文件交换目录。
    transfer_root: Option<PathBuf>,
    /// 本机检测到的能力。
    capabilities: CapabilitySet,
    /// 当前临时控制码。
    pairing_code: Option<String>,
    /// 当前控制码的 Relay 租约到期时间。
    pairing_code_expires_at: Option<DateTime<Utc>>,
    /// 当前活动控制端数量。
    active_connections: usize,
    /// 当前 Owner、Controller 类型和权限绑定。
    controller_bindings: Vec<AgentControllerBinding>,
    /// 与后台运行时共享的 Agent 本地权限控制器。
    permission_control: AgentPermissionControl,
    /// 当前进程是否已提升权限。
    elevated: Option<bool>,
    /// 共享界面翻译器。
    translator: Translator,
    /// 外部语言包路径。
    language_file: Option<PathBuf>,
    /// 是否展示停止确认框。
    show_stop_confirmation: bool,
    /// 是否展示高级设置框。
    show_advanced_settings: bool,
    /// 是否允许本次窗口关闭请求直接执行。
    allow_close: bool,
    /// 复制成功提示的截止时间。
    copied_until: Option<Instant>,
    /// 上一次应用窗口尺寸时是否处于首次配置页。
    layout_is_setup: Option<bool>,
    /// 启动窗口等待稳定外框并完成居中的状态。
    window_centering: WindowCenteringState,
}

/// 启动窗口的居中校准状态。
enum WindowCenteringState {
    /// 等待窗口外框尺寸稳定。
    Waiting {
        /// 是否已记录启动窗口的 DPI 与外框信息。
        metrics_recorded: bool,
        /// 上一帧原生窗口外框尺寸。
        previous_outer_size: Option<Vec2>,
    },
    /// 已使用最终外框完成居中。
    Complete,
}

impl Default for WindowCenteringState {
    fn default() -> Self {
        Self::Waiting {
            metrics_recorded: false,
            previous_outer_size: None,
        }
    }
}

impl RemoteOpsAgentApp {
    /// 创建窗口并启动真实或演示后台。
    fn new(
        cc: &eframe::CreationContext<'_>,
        startup: StartupState,
        requested_language: Option<Language>,
        diagnostics: StartupDiagnostics,
    ) -> Self {
        configure_fonts(&cc.egui_ctx);
        install_style(&cc.egui_ctx);
        let saved_settings = cc
            .storage
            .and_then(|storage| {
                eframe::get_value::<AgentGuiSettings>(storage, "remoteops_agent_gui_settings")
            })
            .unwrap_or_default();
        let language = saved_settings
            .language
            .or(requested_language)
            .unwrap_or_else(Language::detect);
        let language_file = std::env::var_os("REMOTEOPS_LANG_FILE").map(PathBuf::from);
        let mut translator = Translator::new(language);
        if let Some(path) = &language_file {
            let _ = translator.overlay_file(path);
        }
        cc.egui_ctx
            .send_viewport_cmd(ViewportCommand::Title(translator.text("app.agent_title")));
        let (event_sender, events) = mpsc::channel();
        let (trust_sender, trust_events) = mpsc::channel();
        let (shutdown, _) = watch::channel(false);
        let mut app = Self {
            diagnostics,
            event_sender,
            events,
            trust_sender,
            trust_events,
            shutdown,
            worker: None,
            trust_worker: None,
            setup: None,
            config_path: active_agent_config_path(),
            certificate_confirmation: None,
            status: UiStatus::Starting,
            agent_instance_id: None,
            relay: String::new(),
            transfer_root: None,
            capabilities: CapabilitySet::default(),
            pairing_code: None,
            pairing_code_expires_at: None,
            active_connections: 0,
            controller_bindings: Vec::new(),
            permission_control: AgentPermissionControl::default(),
            elevated: detect_elevated(),
            translator,
            language_file,
            show_stop_confirmation: false,
            show_advanced_settings: false,
            allow_close: false,
            copied_until: None,
            layout_is_setup: None,
            window_centering: WindowCenteringState::default(),
        };
        match startup {
            StartupState::Ready {
                config,
                config_path,
                demo: startup_demo,
            } => {
                app.config_path = config_path;
                app.relay.clone_from(&config.relay);
                app.permission_control = AgentPermissionControl::from_config(&config);
                if startup_demo {
                    app.worker = Some(spawn_demo(
                        app.event_sender.clone(),
                        config,
                        app.shutdown.subscribe(),
                    ));
                } else {
                    app.begin_connect(config);
                }
            }
            StartupState::Setup(setup) => {
                app.relay.clone_from(&setup.relay);
                app.config_path.clone_from(&setup.config_path);
                app.setup = Some(setup);
            }
        }
        app
    }

    /// 按当前信任配置启动 TLS 预检或直接启动 Agent。
    fn begin_connect(&mut self, config: AgentConfig) {
        self.relay.clone_from(&config.relay);
        self.status = UiStatus::Connecting;
        self.certificate_confirmation = None;
        if config.ca_cert.is_some() {
            self.start_live(config);
            return;
        }
        self.trust_worker = Some(spawn_trust_bootstrap(self.trust_sender.clone(), config));
    }

    /// 启动真实 Agent 运行时。
    fn start_live(&mut self, config: AgentConfig) {
        self.permission_control = AgentPermissionControl::from_config(&config);
        self.worker = Some(spawn_live(
            self.event_sender.clone(),
            config,
            self.shutdown.subscribe(),
            self.permission_control.clone(),
        ));
    }

    /// 处理 TLS 预检线程返回的状态。
    fn receive_trust_events(&mut self) {
        while let Ok(event) = self.trust_events.try_recv() {
            match event {
                TrustBootstrapEvent::Ready(config) => self.start_live(config),
                TrustBootstrapEvent::ConfirmationRequired {
                    config,
                    fingerprint,
                    reason,
                } => {
                    self.status = UiStatus::Failed(
                        self.translator.text("agent.certificate.confirm_required"),
                    );
                    self.certificate_confirmation = Some(CertificateConfirmation {
                        config,
                        fingerprint,
                        reason,
                        save_error: None,
                    });
                }
                TrustBootstrapEvent::Failed(message) => {
                    self.diagnostics.record("trust_bootstrap_failed");
                    self.diagnostics.record_detail("network_error", &message);
                    self.status = UiStatus::Failed(message);
                }
            }
        }
    }

    /// 将后台事件归并到当前界面快照。
    fn receive_events(&mut self) {
        self.receive_trust_events();
        while let Ok(event) = self.events.try_recv() {
            self.apply_event(event);
        }
    }

    /// 应用单个 Agent 生命周期事件。
    fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Started {
                agent_instance_id,
                relay,
                transfer_root,
                capabilities,
            } => {
                self.agent_instance_id = Some(agent_instance_id);
                self.relay = relay;
                self.transfer_root = Some(transfer_root);
                self.capabilities = capabilities;
                self.status = UiStatus::Connecting;
            }
            AgentEvent::Connecting => self.status = UiStatus::Connecting,
            AgentEvent::Connected {
                pairing_code,
                lease_expires_at,
            } => {
                self.pairing_code = Some(pairing_code);
                self.pairing_code_expires_at = Some(lease_expires_at);
                self.status = if self.active_connections > 0 {
                    UiStatus::Controlled
                } else {
                    UiStatus::Waiting
                };
            }
            AgentEvent::LeaseRenewed { lease_expires_at } => {
                self.pairing_code_expires_at = Some(lease_expires_at);
            }
            AgentEvent::ControllerCountChanged { active_connections } => {
                self.active_connections = active_connections;
                if matches!(self.status, UiStatus::Waiting | UiStatus::Controlled) {
                    self.status = if active_connections > 0 {
                        UiStatus::Controlled
                    } else {
                        UiStatus::Waiting
                    };
                }
            }
            AgentEvent::ControllerBindingsChanged { bindings } => {
                self.controller_bindings = bindings;
            }
            AgentEvent::Reconnecting { message, .. } => {
                self.diagnostics.record("agent_reconnecting");
                self.diagnostics.record_detail("network_error", &message);
                self.active_connections = 0;
                self.status = UiStatus::Reconnecting(message);
            }
            AgentEvent::Stopped => self.status = UiStatus::Stopped,
            AgentEvent::Failed { message } => {
                self.diagnostics.record("agent_failed");
                self.diagnostics.record_detail("agent_error", &message);
                self.status = UiStatus::Failed(message);
            }
        }
    }

    /// 请求后台停止，并允许随后关闭窗口。
    fn stop_and_close(&mut self, ctx: &egui::Context) {
        let _ = self.shutdown.send(true);
        self.status = UiStatus::Stopped;
        self.allow_close = true;
        self.show_stop_confirmation = false;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    /// 切换界面语言，并重新应用可选外部语言包。
    fn set_language(&mut self, language: Language, ctx: &egui::Context) {
        self.translator.set_language(language);
        if let Some(path) = &self.language_file {
            let _ = self.translator.overlay_file(path);
        }
        ctx.send_viewport_cmd(ViewportCommand::Title(
            self.translator.text("app.agent_title"),
        ));
    }

    /// 保存首次配置并开始 TLS 预检。
    fn save_setup_and_connect(&mut self) {
        let Some(setup) = self.setup.as_ref() else {
            return;
        };
        let config = match setup.build_config() {
            Ok(config) => config,
            Err(error) => {
                if let Some(setup) = self.setup.as_mut() {
                    setup.error = Some(error.to_string());
                }
                return;
            }
        };
        let save_result = persist_agent_config(&config, &setup.config_path, !setup.explicit_config);
        match save_result {
            Ok(path) => {
                self.config_path = path;
                self.setup = None;
                self.begin_connect(config);
            }
            Err(error) => {
                if let Some(setup) = self.setup.as_mut() {
                    setup.error = Some(error.to_string());
                }
            }
        }
    }

    /// 渲染首次运行配置页面。
    #[allow(clippy::too_many_lines)]
    fn render_setup(&mut self, ui: &mut egui::Ui) {
        let mut save_clicked = false;
        Frame::new()
            .fill(Color32::from_rgb(246, 249, 253))
            .inner_margin(Margin::symmetric(32, 28))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icons::GEAR)
                            .color(Color32::from_rgb(15, 103, 232))
                            .size(27.0),
                    );
                    ui.label(
                        RichText::new(self.translator.text("agent.setup.title"))
                            .strong()
                            .size(21.0),
                    );
                    let language_width = ui.available_width();
                    ui.allocate_ui_with_layout(
                        Vec2::new(language_width, 32.0),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            for language in [Language::EnUs, Language::ZhCn] {
                                let selected = self.translator.language() == language;
                                if ui
                                    .selectable_label(
                                        selected,
                                        self.translator
                                            .text(&format!("language.{}", language.code())),
                                    )
                                    .clicked()
                                {
                                    self.set_language(language, ui.ctx());
                                }
                            }
                        },
                    );
                });
                ui.add_space(22.0);
                let Some(setup) = self.setup.as_mut() else {
                    return;
                };
                Grid::new("agent-first-run-setup")
                    .num_columns(2)
                    .spacing(Vec2::new(18.0, 14.0))
                    .min_col_width(150.0)
                    .show(ui, |ui| {
                        ui.label(self.translator.text("agent.setup.relay"));
                        ui.add(
                            egui::TextEdit::singleline(&mut setup.relay)
                                .desired_width(f32::INFINITY)
                                .min_size(Vec2::new(0.0, 36.0))
                                .margin(Margin::symmetric(8, 8))
                                .hint_text("relay.example.com:7443"),
                        );
                        ui.end_row();
                    });
                ui.add_space(18.0);
                ui.label(
                    RichText::new(self.translator.text_with(
                        "agent.setup.config_path",
                        &[("path", &setup.config_path.display().to_string())],
                    ))
                    .color(Color32::from_rgb(100, 116, 139))
                    .size(12.0),
                );
                if let Some(error) = &setup.error {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(setup_error_message(error))
                            .color(Color32::from_rgb(190, 38, 51))
                            .size(13.0),
                    );
                }
                ui.add_space(22.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    save_clicked = ui
                        .add_sized(
                            [150.0, 38.0],
                            egui::Button::new(
                                RichText::new(self.translator.text("agent.setup.save_connect"))
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(15, 103, 232))
                            .corner_radius(8.0),
                        )
                        .clicked();
                });
            });
        if save_clicked {
            self.save_setup_and_connect();
        }
    }

    /// 渲染未知或变化证书的本地确认窗口。
    fn render_certificate_confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirmation) = self.certificate_confirmation.as_ref() else {
            return;
        };
        let mut action = None;
        egui::Window::new(self.translator.text("agent.certificate.title"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(self.translator.text("agent.certificate.warning"))
                        .color(Color32::from_rgb(190, 38, 51))
                        .strong(),
                );
                ui.add_space(10.0);
                ui.label(self.translator.text_with(
                    "agent.certificate.relay",
                    &[("relay", &confirmation.config.relay)],
                ));
                ui.label(self.translator.text("agent.certificate.fingerprint"));
                ui.monospace(&confirmation.fingerprint);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(&confirmation.reason)
                        .color(Color32::from_rgb(100, 116, 139))
                        .size(12.0),
                );
                if let Some(error) = &confirmation.save_error {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(error)
                            .color(Color32::from_rgb(190, 38, 51))
                            .size(12.0),
                    );
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(self.translator.text("agent.certificate.cancel"))
                        .clicked()
                    {
                        action = Some(CertificateAction::Cancel);
                    }
                    if ui
                        .button(self.translator.text("agent.certificate.continue_once"))
                        .clicked()
                    {
                        action = Some(CertificateAction::ContinueOnce);
                    }
                    if ui
                        .button(self.translator.text("agent.certificate.trust_save"))
                        .clicked()
                    {
                        action = Some(CertificateAction::TrustAndSave);
                    }
                });
            });
        if let Some(action) = action {
            self.apply_certificate_action(action);
        }
    }

    /// 应用本地用户对未知证书的决定。
    fn apply_certificate_action(&mut self, action: CertificateAction) {
        let Some(mut confirmation) = self.certificate_confirmation.take() else {
            return;
        };
        match action {
            CertificateAction::Cancel => {
                self.status = UiStatus::Failed(self.translator.text("agent.certificate.cancelled"));
            }
            CertificateAction::ContinueOnce => {
                confirmation.config.tls_fingerprint = Some(confirmation.fingerprint.clone());
                self.begin_connect(confirmation.config);
            }
            CertificateAction::TrustAndSave => {
                confirmation.config.tls_fingerprint = Some(confirmation.fingerprint.clone());
                match persist_agent_config(&confirmation.config, &self.config_path, true) {
                    Ok(path) => {
                        self.config_path = path;
                        self.begin_connect(confirmation.config);
                    }
                    Err(error) => {
                        confirmation.save_error = Some(error.to_string());
                        self.certificate_confirmation = Some(confirmation);
                    }
                }
            }
        }
    }

    /// 渲染当前服务状态、控制码和工程师连接状态。
    fn render_status_card(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(Color32::WHITE)
            .inner_margin(Margin::symmetric(20, 5))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        let content_width = 24.0
                            + ui.spacing().item_spacing.x
                            + ui.painter()
                                .layout_no_wrap(
                                    self.primary_status_label(),
                                    FontId::proportional(16.0),
                                    self.status.color(),
                                )
                                .size()
                                .x;
                        ui.add_space(centered_left_padding(ui.available_width(), content_width));
                        ui.label(
                            RichText::new(icons::CHECK_CIRCLE)
                                .color(self.status.color())
                                .size(21.0),
                        );
                        ui.label(
                            RichText::new(self.primary_status_label())
                                .color(self.status.color())
                                .strong()
                                .size(16.0),
                        );
                    });
                });
                if let Some(detail) = self.status.detail() {
                    ui.vertical_centered(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(detail)
                                    .color(Color32::from_rgb(100, 116, 139))
                                    .size(11.0),
                            )
                            .truncate(),
                        )
                        .on_hover_text(detail);
                    });
                }
                ui.add_space(3.0);
                ui.vertical_centered(|ui| {
                    self.render_pairing_code_row(ui);
                    ui.label(
                        RichText::new(self.pairing_code_expiry_label())
                            .color(Color32::from_rgb(71, 85, 105))
                            .size(12.0),
                    );
                });
                ui.add_space(3.0);
                ui.separator();
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let label = self.controller_status_label();
                    let content_width = 21.0
                        + ui.spacing().item_spacing.x
                        + ui.painter()
                            .layout_no_wrap(
                                label.clone(),
                                FontId::proportional(14.0),
                                Color32::from_rgb(30, 64, 108),
                            )
                            .size()
                            .x;
                    ui.add_space(centered_left_padding(ui.available_width(), content_width));
                    ui.label(
                        RichText::new(if self.active_connections > 0 {
                            icons::USER_CHECK
                        } else {
                            icons::USERS
                        })
                        .color(Color32::from_rgb(30, 64, 108))
                        .size(19.0),
                    );
                    ui.label(
                        RichText::new(label)
                            .color(Color32::from_rgb(30, 64, 108))
                            .strong()
                            .size(14.0),
                    );
                });
            });
    }

    /// 返回主状态区使用的简短文案。
    fn primary_status_label(&self) -> String {
        if matches!(self.status, UiStatus::Waiting | UiStatus::Controlled) {
            self.translator.text("agent.status.ready")
        } else {
            self.status.label(&self.translator)
        }
    }

    /// 返回当前控制码的租约倒计时文案。
    fn pairing_code_expiry_label(&self) -> String {
        let Some(expires_at) = self.pairing_code_expires_at.as_ref() else {
            return self.translator.text("status.pairing_code_ephemeral");
        };
        let Some(seconds) = pairing_code_remaining_seconds(expires_at, &Utc::now()) else {
            return self.translator.text("agent.pairing_code.expired");
        };
        self.translator.text_with(
            "agent.pairing_code.expires_in",
            &[("time", &format_countdown(seconds))],
        )
    }

    /// 判断当前控制码是否仍可复制。
    fn pairing_code_is_active(&self) -> bool {
        self.pairing_code.is_some()
            && self
                .pairing_code_expires_at
                .as_ref()
                .is_none_or(|expires_at| {
                    pairing_code_remaining_seconds(expires_at, &Utc::now()).is_some()
                })
    }

    /// 将控制码和复制按钮作为一个整体水平居中展示。
    fn render_pairing_code_row(&mut self, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            pairing_code_row_size(ui.available_width()),
            Layout::left_to_right(Align::Center),
            |ui| {
                let code = self
                    .pairing_code
                    .as_deref()
                    .map_or_else(|| "--- --- ---".to_owned(), display_pairing_code);
                let code_active = self.pairing_code_is_active();
                let code_color = if code_active {
                    Color32::from_rgb(30, 41, 59)
                } else {
                    Color32::from_rgb(148, 163, 184)
                };
                let code_width = ui
                    .painter()
                    .layout_no_wrap(code.clone(), FontId::monospace(36.0), code_color)
                    .size()
                    .x;
                ui.add_space(pairing_code_left_padding(
                    ui.available_width(),
                    code_width,
                    ui.spacing().item_spacing.x,
                ));
                ui.label(RichText::new(code).color(code_color).size(32.0).monospace());
                let copied = self
                    .copied_until
                    .is_some_and(|deadline| deadline > Instant::now());
                let icon = if copied { icons::CHECK } else { icons::COPY };
                let response = ui
                    .scope(|ui| {
                        stabilize_copy_button_style(ui.style_mut());
                        ui.add_enabled(
                            code_active,
                            egui::Button::new(
                                RichText::new(icon)
                                    .color(Color32::from_rgb(51, 65, 85))
                                    .size(18.0),
                            )
                            .fill(Color32::from_rgb(248, 250, 252))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(203, 213, 225)))
                            .corner_radius(4.0)
                            .min_size(Vec2::splat(COPY_CODE_BUTTON_SIZE)),
                        )
                    })
                    .inner
                    .on_hover_text(self.translator.text("agent.action.copy"));
                if response.clicked()
                    && let Some(pairing_code) = &self.pairing_code
                {
                    ui.ctx().copy_text(pairing_code.clone());
                    self.copied_until = Some(Instant::now() + Duration::from_secs(3));
                }
            },
        );
    }

    /// 返回当前已绑定控制端的简短状态。
    fn controller_status_label(&self) -> String {
        match self.active_connections {
            0 => self.translator.text("agent.engineer.waiting"),
            _ => self.translator.text("status.controlled"),
        }
    }

    /// 渲染紧凑的能力可用性摘要。
    fn render_capabilities(&self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(Color32::WHITE)
            .inner_margin(Margin::symmetric(20, 4))
            .show(ui, |ui| {
                ui.columns(4, |columns| {
                    self.render_capability(
                        &mut columns[0],
                        icons::TERMINAL_WINDOW,
                        &self.translator.text("agent.capability.command_short"),
                        self.has_shell_capability(),
                    );
                    self.render_capability(
                        &mut columns[1],
                        icons::FOLDER_OPEN,
                        &self.translator.text("agent.capability.file_short"),
                        self.capabilities.contains(Capability::FileTransfer),
                    );
                    self.render_capability(
                        &mut columns[2],
                        icons::DESKTOP,
                        &self.translator.text("agent.capability.ssh_short"),
                        self.capabilities.contains(Capability::Ssh),
                    );
                    self.render_capability(
                        &mut columns[3],
                        icons::PLUGS_CONNECTED,
                        &self.translator.text("agent.capability.serial_short"),
                        self.capabilities.contains(Capability::Serial),
                    );
                });
            });
    }

    /// 渲染只读运行信息；SSH 密码只在控制端 MCP 的本机安全窗口录入。
    #[allow(clippy::too_many_lines)]
    fn render_advanced_settings(&mut self, ctx: &egui::Context) {
        if !self.show_advanced_settings {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("agent-advanced-settings"))
            .backdrop_color(Color32::from_black_alpha(90))
            .frame(
                Frame::new()
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(226, 232, 240)))
                    .corner_radius(8.0)
                    .inner_margin(Margin::symmetric(20, 18))
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 2,
                        color: Color32::from_black_alpha(45),
                    }),
            );
        modal.show(ctx, |ui| {
            ui.set_min_width(400.0);
            ui.set_max_width(420.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icons::GEAR)
                        .size(22.0)
                        .color(Color32::from_rgb(51, 65, 85)),
                );
                ui.label(
                    RichText::new(self.translator.text("agent.advanced.title"))
                        .size(19.0)
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(RichText::new(icons::X).size(17.0))
                        .on_hover_text(self.translator.text("agent.advanced.close"))
                        .clicked()
                    {
                        self.show_advanced_settings = false;
                    }
                });
            });
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            Self::render_detail_line(
                ui,
                icons::CLOUD,
                &self.translator.text("agent.advanced.relay"),
                self.relay.clone(),
            );
            Self::render_detail_line(
                ui,
                icons::SHIELD_CHECK,
                &self.translator.text("agent.advanced.elevation"),
                elevation_label(self.elevated, self.translator.language()),
            );
            let transfer_root = self.transfer_root.as_ref().map_or_else(
                || self.translator.text("agent.advanced.not_initialized"),
                |path| path.display().to_string(),
            );
            Self::render_detail_line(
                ui,
                icons::FOLDER_OPEN,
                &self.translator.text("agent.advanced.transfer_root"),
                transfer_root,
            );
        });
    }

    /// 渲染一行紧凑运行详情，长值通过悬停查看全文。
    fn render_detail_line(ui: &mut egui::Ui, icon: &str, label: &str, value: String) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icon)
                    .size(14.0)
                    .color(Color32::from_rgb(71, 85, 105)),
            );
            ui.label(
                RichText::new(format!("{label}:"))
                    .size(12.0)
                    .color(Color32::from_rgb(71, 85, 105)),
            );
            ui.add(egui::Label::new(RichText::new(&value).size(12.0)).truncate())
                .on_hover_text(value);
        });
    }

    /// 渲染单个能力项。
    fn render_capability(&self, ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool) {
        let color = if enabled {
            Color32::from_rgb(30, 41, 59)
        } else {
            Color32::from_rgb(148, 163, 184)
        };
        let status = if enabled {
            self.translator.text("agent.capability.enabled")
        } else {
            self.translator.text("agent.capability.disabled")
        };
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(icon).size(20.0).color(color))
                .on_hover_text(&status);
            ui.label(RichText::new(label).strong().size(11.0).color(color))
                .on_hover_text(status);
        });
    }

    /// 判断是否存在任一命令 Shell 能力。
    fn has_shell_capability(&self) -> bool {
        [
            Capability::Cmd,
            Capability::WindowsPowerShell,
            Capability::PowerShell,
        ]
        .into_iter()
        .any(|capability| self.capabilities.contains(capability))
    }

    /// 渲染高级设置与停止协助操作栏。
    fn render_action_bar(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(Color32::WHITE)
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    if ui
                        .button(format!(
                            "{}  {}",
                            icons::GEAR,
                            self.translator.text("agent.action.advanced_settings")
                        ))
                        .clicked()
                    {
                        self.show_advanced_settings = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let stop = egui::Button::new(
                            RichText::new(format!(
                                "{}  {}",
                                icons::STOP_CIRCLE,
                                self.translator.text("agent.action.stop_remote")
                            ))
                            .color(Color32::from_rgb(220, 38, 38))
                            .strong(),
                        )
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, Color32::from_rgb(239, 68, 68)))
                        .corner_radius(6.0)
                        .min_size(Vec2::new(190.0, 38.0));
                        if ui.add(stop).clicked() {
                            self.show_stop_confirmation = true;
                        }
                    });
                });
            });
    }

    /// 渲染停止确认框。
    fn render_stop_confirmation(&mut self, ctx: &egui::Context) {
        if !self.show_stop_confirmation {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("agent-stop-confirmation"))
            .backdrop_color(Color32::from_black_alpha(120))
            .frame(
                Frame::new()
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(226, 232, 240)))
                    .corner_radius(14.0)
                    .inner_margin(Margin::symmetric(20, 18))
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 2,
                        color: Color32::from_black_alpha(55),
                    }),
            );
        let response = modal.show(ctx, |ui| {
            ui.set_min_width(380.0);
            ui.set_max_width(400.0);
            ui.horizontal(|ui| {
                ui.add_sized(
                    [46.0, 46.0],
                    egui::Label::new(
                        RichText::new(icons::WARNING)
                            .size(32.0)
                            .color(Color32::from_rgb(220, 38, 38)),
                    ),
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(self.translator.text("agent.stop.title"))
                            .size(20.0)
                            .strong()
                            .color(Color32::from_rgb(15, 23, 42)),
                    );
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(self.translator.text("agent.stop.subtitle"))
                            .size(13.0)
                            .color(Color32::from_rgb(100, 116, 139)),
                    );
                });
            });
            ui.add_space(14.0);
            Frame::new()
                .fill(Color32::from_rgb(248, 250, 252))
                .stroke(Stroke::new(1.0, Color32::from_rgb(226, 232, 240)))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(self.translator.text("agent.stop.message"))
                            .size(14.0)
                            .color(Color32::from_rgb(51, 65, 85)),
                    );
                });
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_sized(
                        [128.0, 40.0],
                        egui::Button::new(
                            RichText::new(self.translator.text("agent.stop.confirm"))
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .fill(Color32::from_rgb(220, 38, 38))
                        .corner_radius(8.0),
                    )
                    .clicked()
                {
                    self.stop_and_close(ctx);
                }
                ui.add_space(8.0);
                if ui
                    .add_sized(
                        [136.0, 40.0],
                        egui::Button::new(
                            RichText::new(self.translator.text("agent.stop.cancel"))
                                .color(Color32::from_rgb(30, 41, 59))
                                .strong(),
                        )
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, Color32::from_rgb(203, 213, 225)))
                        .corner_radius(8.0),
                    )
                    .clicked()
                {
                    self.show_stop_confirmation = false;
                }
            });
        });
        if response.should_close() {
            self.show_stop_confirmation = false;
        }
    }
}

/// 把首次配置错误整理为适合紧凑界面展示的单行提示。
fn setup_error_message(error: &str) -> String {
    if error.contains("Agent 配置文件不存在：") {
        return "Agent 配置文件不存在，请确认上方路径后重试。".to_owned();
    }
    error.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 将控制码统一显示为连字符分隔的三位一组，和复制值保持一致。
fn display_pairing_code(code: &str) -> String {
    let digits: String = code.chars().filter(char::is_ascii_digit).collect();
    if digits.len() == 9 {
        return format!("{}-{}-{}", &digits[..3], &digits[3..6], &digits[6..]);
    }
    code.to_owned()
}

/// 返回控制码距离租约到期的剩余秒数；已过期时返回 None。
fn pairing_code_remaining_seconds(expires_at: &DateTime<Utc>, now: &DateTime<Utc>) -> Option<i64> {
    let seconds = (*expires_at - *now).num_seconds();
    (seconds > 0).then_some(seconds)
}

/// 格式化控制码倒计时，最多显示到小时级别。
fn format_countdown(seconds: i64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// 返回适合当前界面语言的短标签。
fn localized_label(language: Language, zh_cn: &'static str, en_us: &'static str) -> &'static str {
    match language {
        Language::ZhCn => zh_cn,
        Language::EnUs => en_us,
    }
}

/// 返回进程提升状态的界面文本。
fn elevation_label(elevated: Option<bool>, language: Language) -> String {
    match elevated {
        Some(true) => localized_label(language, "已提升", "Elevated"),
        Some(false) => localized_label(language, "未提升", "Not elevated"),
        None => localized_label(language, "未知", "Unknown"),
    }
    .to_owned()
}

/// 检测当前 GUI 进程是否已提升权限，并避免创建可见控制台窗口。
fn detect_elevated() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;

        let output = std::process::Command::new("whoami.exe")
            .args(["/groups", "/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let groups = String::from_utf8_lossy(&output.stdout);
        Some(groups.contains("S-1-16-12288") || groups.contains("S-1-16-16384"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|user_id| user_id.trim() == "0")
    }
}

/// 根据显示器和窗口最终外框计算屏幕中心位置。
fn centered_window_position(monitor_size: Vec2, outer_size: Vec2) -> egui::Pos2 {
    egui::pos2(
        ((monitor_size.x - outer_size.x) / 2.0).max(0.0),
        ((monitor_size.y - outer_size.y) / 2.0).max(0.0),
    )
}

/// 判断窗口外框是否已经完成启动阶段的尺寸调整。
fn window_size_is_stable(previous: Vec2, current: Vec2) -> bool {
    (previous.x - current.x).abs() <= 1.0 && (previous.y - current.y).abs() <= 1.0
}

impl eframe::App for RemoteOpsAgentApp {
    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let is_setup = self.setup.is_some();
        let (required_size, background) = if is_setup {
            (SETUP_WINDOW_SIZE, Color32::from_rgb(246, 249, 253))
        } else {
            (RUNNING_WINDOW_SIZE, Color32::WHITE)
        };
        if self.layout_is_setup != Some(is_setup) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(required_size));
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(required_size));
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(required_size));
            ctx.send_viewport_cmd(ViewportCommand::Resizable(false));
            ctx.send_viewport_cmd(ViewportCommand::EnableButtons {
                close: true,
                minimized: true,
                maximize: false,
            });
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
            self.layout_is_setup = Some(is_setup);
            self.window_centering = WindowCenteringState::default();
        }
        if let WindowCenteringState::Waiting {
            metrics_recorded,
            previous_outer_size,
        } = &mut self.window_centering
        {
            let metrics = ctx.input(|input| {
                let viewport = input.viewport();
                Some((
                    viewport.native_pixels_per_point?,
                    viewport.monitor_size?,
                    viewport.outer_rect?,
                ))
            });
            if !*metrics_recorded && let Some((scale, monitor_size, outer_rect)) = metrics {
                self.diagnostics.record_detail(
                    "window_metrics",
                    format!("scale={scale}; monitor={monitor_size:?}; outer={outer_rect:?}"),
                );
                *metrics_recorded = true;
            }
            let viewport_metrics = ctx.input(|input| {
                let viewport = input.viewport();
                Some((
                    viewport.monitor_size?,
                    viewport.inner_rect?.size(),
                    viewport.outer_rect?.size(),
                ))
            });
            if let Some((monitor_size, inner_size, outer_size)) = viewport_metrics {
                if !window_size_is_stable(required_size, inner_size) {
                    *previous_outer_size = None;
                    ctx.request_repaint_after(Duration::from_millis(16));
                } else if previous_outer_size
                    .is_some_and(|previous| window_size_is_stable(previous, outer_size))
                {
                    let position = centered_window_position(monitor_size, outer_size);
                    ctx.send_viewport_cmd(ViewportCommand::OuterPosition(position));
                    self.diagnostics.record_detail(
                        "window_centered",
                        format!("position={position:?}; outer_size={outer_size:?}"),
                    );
                    self.window_centering = WindowCenteringState::Complete;
                } else {
                    *previous_outer_size = Some(outer_size);
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
            }
        }
        if is_setup {
            if ctx.input(|input| input.viewport().close_requested()) {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            egui::CentralPanel::default()
                .frame(Frame::new().fill(background).inner_margin(Margin::ZERO))
                .show(ui, |ui| self.render_setup(ui));
            ctx.request_repaint_after(Duration::from_millis(250));
            return;
        }
        self.receive_events();
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if matches!(self.status, UiStatus::Stopped | UiStatus::Failed(_)) {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                self.show_stop_confirmation = true;
            }
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(Color32::WHITE).inner_margin(Margin::ZERO))
            .show(ui, |ui| {
                Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(Margin::symmetric(14, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(icons::SHIELD_CHECK)
                                    .color(Color32::from_rgb(13, 110, 253))
                                    .size(19.0),
                            );
                            ui.label(
                                RichText::new(self.translator.text("app.agent_title"))
                                    .strong()
                                    .size(15.0),
                            );
                            let language_width = ui.available_width();
                            ui.allocate_ui_with_layout(
                                Vec2::new(language_width, 24.0),
                                Layout::right_to_left(Align::Center),
                                |ui| {
                                    for (index, language) in
                                        [Language::EnUs, Language::ZhCn].into_iter().enumerate()
                                    {
                                        let selected = self.translator.language() == language;
                                        if ui
                                            .selectable_label(
                                                selected,
                                                self.translator.text(&format!(
                                                    "language.{}.short",
                                                    language.code()
                                                )),
                                            )
                                            .on_hover_text(
                                                self.translator.text("language.switch_hint"),
                                            )
                                            .clicked()
                                        {
                                            self.set_language(language, &ctx);
                                        }
                                        if index == 0 {
                                            ui.label(
                                                RichText::new("|")
                                                    .color(Color32::from_rgb(148, 163, 184)),
                                            );
                                        }
                                    }
                                },
                            );
                        });
                    });
                ui.separator();
                self.render_status_card(ui);
                ui.separator();
                self.render_capabilities(ui);
                ui.separator();
                self.render_action_bar(ui);
            });
        self.render_stop_confirmation(&ctx);
        self.render_advanced_settings(&ctx);
        self.render_certificate_confirmation(&ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            "remoteops_agent_gui_settings",
            &AgentGuiSettings {
                language: Some(self.translator.language()),
            },
        );
    }
}

impl Drop for RemoteOpsAgentApp {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

fn persist_agent_config(
    config: &AgentConfig,
    requested_path: &std::path::Path,
    allow_legacy_fallback: bool,
) -> anyhow::Result<PathBuf> {
    persist_agent_config_with(requested_path, allow_legacy_fallback, |path| {
        config.save_file(path)
    })
}

fn persist_agent_config_with(
    requested_path: &std::path::Path,
    allow_legacy_fallback: bool,
    mut save: impl FnMut(&std::path::Path) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    match save(requested_path) {
        Ok(()) => Ok(requested_path.to_path_buf()),
        Err(primary_error)
            if allow_legacy_fallback && requested_path == default_agent_config_path() =>
        {
            let fallback = legacy_agent_config_path();
            save(&fallback).map_err(|fallback_error| {
                anyhow::anyhow!(
                    "无法保存配置到 {}：{primary_error:#}；回退到 {} 也失败：{fallback_error:#}",
                    requested_path.display(),
                    fallback.display()
                )
            })?;
            Ok(fallback)
        }
        Err(error) => Err(error),
    }
}

/// 在发送任何 `RemoteOps` 凭据前检查系统信任链或固定证书指纹。
fn spawn_trust_bootstrap(
    sender: Sender<TrustBootstrapEvent>,
    config: AgentConfig,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("remoteops-agent-tls-bootstrap".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let event = match runtime {
                Ok(runtime) => runtime.block_on(async {
                    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
                        match load_pinned_client_config(
                            &config.relay,
                            &config.server_name,
                            fingerprint,
                        )
                        .await
                        {
                            Ok(_) => TrustBootstrapEvent::Ready(config),
                            Err(error) => {
                                match probe_server_certificate(&config.relay, &config.server_name)
                                    .await
                                {
                                    Ok(probe) => TrustBootstrapEvent::ConfirmationRequired {
                                        config,
                                        fingerprint: probe.sha256_fingerprint,
                                        reason: error.to_string(),
                                    },
                                    Err(_) => TrustBootstrapEvent::Ready(config),
                                }
                            }
                        }
                    } else {
                        let native_result = match load_native_client_config() {
                            Ok(client_config) => {
                                connect_tls(&config.relay, &config.server_name, client_config)
                                    .await
                                    .map(|_| ())
                                    .map_err(|error| error.to_string())
                            }
                            Err(error) => Err(error.to_string()),
                        };
                        match native_result {
                            Ok(()) => TrustBootstrapEvent::Ready(config),
                            Err(reason) => {
                                match probe_server_certificate(&config.relay, &config.server_name)
                                    .await
                                {
                                    Ok(probe) => TrustBootstrapEvent::ConfirmationRequired {
                                        config,
                                        fingerprint: probe.sha256_fingerprint,
                                        reason,
                                    },
                                    Err(_) => TrustBootstrapEvent::Ready(config),
                                }
                            }
                        }
                    }
                }),
                Err(error) => {
                    TrustBootstrapEvent::Failed(format!("无法创建 TLS 预检运行时：{error}"))
                }
            };
            let _ = sender.send(event);
        })
        .expect("应能启动 TLS 预检线程")
}

/// 启动真实 Agent 后台线程。
fn spawn_live(
    event_sender: AgentEventSender,
    config: AgentConfig,
    shutdown_receiver: watch::Receiver<bool>,
    permission_control: AgentPermissionControl,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("remoteops-agent-runtime".to_owned())
        .spawn(move || {
            initialize_tracing();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .thread_name("remoteops-agent-worker")
                .build();
            let result = match runtime {
                Ok(runtime) => runtime.block_on(run_agent_with_permission_control(
                    config,
                    Some(event_sender.clone()),
                    shutdown_receiver,
                    permission_control,
                )),
                Err(error) => Err(anyhow::Error::new(error).context("无法创建 Agent 异步运行时")),
            };
            if let Err(error) = result {
                let _ = event_sender.send(AgentEvent::Failed {
                    message: error.to_string(),
                });
            }
        })
        .expect("应能启动 Agent 后台线程")
}

/// 启动不连接网络的视觉验收后台。
fn spawn_demo(
    event_sender: AgentEventSender,
    config: AgentConfig,
    _shutdown_receiver: watch::Receiver<bool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("remoteops-agent-demo".to_owned())
        .spawn(move || {
            let _ = event_sender.send(AgentEvent::Started {
                agent_instance_id: AgentInstanceId::new(),
                relay: config.relay,
                transfer_root: config.transfer_root,
                capabilities: CapabilitySet::new([
                    Capability::WindowsPowerShell,
                    Capability::Ssh,
                    Capability::Serial,
                    Capability::FileTransfer,
                ]),
            });
            std::thread::sleep(Duration::from_millis(450));
            let _ = event_sender.send(AgentEvent::Connected {
                pairing_code: "482-915-307".to_owned(),
                lease_expires_at: Utc::now() + chrono::Duration::minutes(10),
            });
            let _ = event_sender.send(AgentEvent::ControllerCountChanged {
                active_connections: 0,
            });
        })
        .expect("应能启动 Agent 演示线程")
}

/// 配置中文和 Phosphor 图标字体。
fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for candidate in [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(candidate) {
            fonts.font_data.insert(
                "remoteops_zh".to_owned(),
                egui::FontData::from_owned(bytes).into(),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "remoteops_zh".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "remoteops_zh".to_owned());
            break;
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

/// 应用简洁、清晰的 Windows 工具型界面样式。
fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Light);
    let theme = ctx.theme();
    let mut style = (*ctx.style_of(theme)).clone();
    style.animation_time = 0.18;
    style.spacing.item_spacing = Vec2::new(7.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.interact_size = Vec2::new(36.0, 34.0);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.visuals.panel_fill = Color32::WHITE;
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.override_text_color = Some(Color32::from_rgb(15, 23, 42));
    style.visuals.selection.bg_fill = Color32::from_rgb(219, 234, 254);
    style.visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(37, 112, 232));
    style.visuals.widgets.inactive.corner_radius = 4.0.into();
    style.visuals.widgets.hovered.corner_radius = 4.0.into();
    style.visuals.widgets.active.corner_radius = 4.0.into();
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(241, 245, 249);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(219, 234, 254);
    ctx.set_style_of(theme, style);
}

/// 安装 GUI 进程级 panic 记录器，保留默认终端输出行为。
fn install_panic_reporter() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        write_panic_report(panic_info);
        previous_hook(panic_info);
    }));
}

/// 返回按优先级排列的本地诊断目录。
fn diagnostic_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(directory) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("logs")))
    {
        directories.push(directory);
    }
    if let Some(directory) = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("RemoteOps").join("logs"))
        && !directories.contains(&directory)
    {
        directories.push(directory);
    }
    let fallback = std::env::temp_dir().join("RemoteOps").join("logs");
    if !directories.contains(&fallback) {
        directories.push(fallback);
    }
    directories
}

/// 把诊断文本写入第一个可用目录。
fn write_diagnostic_report(file_name: &str, report: &str) -> Option<PathBuf> {
    for directory in diagnostic_directories() {
        if std::fs::create_dir_all(&directory).is_ok() {
            let path = directory.join(file_name);
            if std::fs::write(&path, report).is_ok() {
                return Some(path);
            }
        }
    }
    None
}

/// 记录 GUI 初始化阶段，帮助定位原生窗口回调中的异常。
#[derive(Clone)]
struct StartupDiagnostics {
    /// 当前运行诊断文件路径。
    path: Option<PathBuf>,
}

impl StartupDiagnostics {
    /// 创建当前进程的启动诊断文件。
    fn start() -> Self {
        let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
        let file_name = format!("agent-gui-runtime-{timestamp}-{}.log", std::process::id());
        let report = format!(
            "RemoteOps Agent GUI runtime diagnostics\nversion: {}\ntime_utc: {}\nprocess_id: {}\nstage: process_started\n",
            env!("CARGO_PKG_VERSION"),
            Utc::now().to_rfc3339(),
            std::process::id()
        );
        Self {
            path: write_diagnostic_report(&file_name, &report),
        }
    }

    /// 追加不包含配置和凭据的启动阶段标记。
    fn record(&self, stage: &str) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
            return;
        };
        let _ = writeln!(file, "time_utc: {}", Utc::now().to_rfc3339());
        let _ = writeln!(file, "stage: {stage}");
    }

    /// 追加已约束为本地枚举或图形适配器元数据的诊断字段。
    fn record_detail(&self, key: &str, value: impl std::fmt::Display) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
            return;
        };
        let value = value.to_string().replace(['\r', '\n'], " ");
        let _ = writeln!(file, "{key}: {value}");
    }

    /// 记录原生窗口初始化返回的错误，避免无控制台构建丢失根因。
    fn record_native_window_error(&self, error: &eframe::Error) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
            return;
        };
        let _ = writeln!(file, "time_utc: {}", Utc::now().to_rfc3339());
        let _ = writeln!(file, "stage: native_window_returned_error");
        let _ = write!(file, "{}", format_native_window_error(error));
    }
}

/// Windows 保留无需 OpenGL 2.0 的 WGPU，其他平台使用 Glow。
fn default_renderer() -> eframe::Renderer {
    if cfg!(target_os = "windows") {
        eframe::Renderer::Wgpu
    } else {
        eframe::Renderer::Glow
    }
}

/// 把 WGPU 限制到 Windows 原生 DirectX 12，允许系统在无显卡时选择软件适配器。
#[cfg(target_os = "windows")]
fn configure_windows_wgpu(options: &mut eframe::NativeOptions) {
    options.wgpu_options.surface = eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT;
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        setup.power_preference = eframe::wgpu::PowerPreference::LowPower;
    }
}

/// 在没有控制台的正式构建中显示启动失败原因和日志位置。
#[cfg(target_os = "windows")]
fn show_native_startup_error(
    renderer: eframe::Renderer,
    error: &eframe::Error,
    diagnostics: &StartupDiagnostics,
) {
    let log_path = diagnostics.path.as_ref().map_or_else(
        || "未能创建诊断日志".to_owned(),
        |path| path.display().to_string(),
    );
    let description = format!(
        "RemoteOps Agent GUI 启动失败。\n\n渲染器：{renderer}\n错误：{error}\n\n诊断日志：{log_path}"
    );
    let _ = rfd::MessageDialog::new()
        .set_title("RemoteOps Agent GUI 启动失败")
        .set_description(description)
        .set_level(rfd::MessageLevel::Error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// 非 Windows 平台保留错误返回，不额外引入系统对话框行为。
#[cfg(not(target_os = "windows"))]
fn show_native_startup_error(
    _renderer: eframe::Renderer,
    _error: &eframe::Error,
    _diagnostics: &StartupDiagnostics,
) {
}

/// 格式化原生窗口错误，同时保留用户可读信息和具体错误类型。
fn format_native_window_error(error: &eframe::Error) -> String {
    format!("error_display: {error}\nerror_debug: {error:?}\n")
}

/// 将窗口回调中的 panic 写入 EXE 同级诊断目录。
fn write_panic_report(panic_info: &std::panic::PanicHookInfo<'_>) {
    let payload = panic_info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| {
            panic_info
                .payload()
                .downcast_ref::<String>()
                .map(String::as_str)
        })
        .unwrap_or("<non-string panic payload>");
    let location = panic_info.location().map_or_else(
        || "<unknown>".to_owned(),
        |location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        },
    );
    let mut report = String::new();
    let _ = writeln!(report, "RemoteOps Agent GUI panic report");
    let _ = writeln!(report, "version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(report, "time_utc: {}", Utc::now().to_rfc3339());
    let _ = writeln!(report, "process_id: {}", std::process::id());
    let _ = writeln!(report, "location: {location}");
    let _ = writeln!(report, "message: {payload}");
    let _ = writeln!(report, "backtrace:\n{}", Backtrace::force_capture());

    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let file_name = format!("agent-gui-crash-{timestamp}-{}.log", std::process::id());
    let _ = write_diagnostic_report(&file_name, &report);
}

/// 创建 Windows Agent GUI 使用的原生窗口配置。
fn agent_native_options(renderer: eframe::Renderer, initial_setup: bool) -> eframe::NativeOptions {
    let initial_size = if initial_setup {
        SETUP_WINDOW_SIZE
    } else {
        RUNNING_WINDOW_SIZE
    };
    #[allow(unused_mut)] // 仅 Windows 配置 WGPU 和事件循环时需要可变。
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("remoteops-agent-gui")
            .with_inner_size(initial_size)
            .with_min_inner_size(initial_size)
            .with_max_inner_size(initial_size)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_drag_and_drop(false),
        centered: true,
        run_and_return: true,
        persist_window: false,
        renderer,
        ..Default::default()
    };
    #[cfg(target_os = "windows")]
    {
        if renderer == eframe::Renderer::Wgpu {
            configure_windows_wgpu(&mut options);
        }
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows as _;

            builder.with_any_thread(true);
        }));
    }
    options
}

/// 在当前线程运行完整 GUI 生命周期，并返回明确的进程退出状态。
fn run_gui(args: Args, diagnostics: &StartupDiagnostics) -> std::process::ExitCode {
    diagnostics.record("ui_thread_started");
    let renderer = args.renderer;
    let requested_language = args.lang;
    let startup = args.into_startup();
    let initial_setup = matches!(&startup, StartupState::Setup(_));
    diagnostics.record_detail("renderer", renderer);
    let native_options = agent_native_options(renderer, initial_setup);
    diagnostics.record("event_loop_configured");
    let mut translator = Translator::detect();
    if let Ok(path) = std::env::var("REMOTEOPS_LANG_FILE") {
        let _ = translator.overlay_file(path);
    }
    let title = translator.text("app.agent_title");
    let app_diagnostics = diagnostics.clone();
    diagnostics.record("starting_native_window");
    let result = eframe::run_native(
        &title,
        native_options,
        Box::new(move |cc| {
            app_diagnostics.record("app_creator_started");
            if let Some(render_state) = &cc.wgpu_render_state {
                let adapter = render_state.adapter.get_info();
                app_diagnostics.record_detail("wgpu_backend", format!("{:?}", adapter.backend));
                app_diagnostics
                    .record_detail("wgpu_device_type", format!("{:?}", adapter.device_type));
                app_diagnostics.record_detail("wgpu_adapter_name", &adapter.name);
                app_diagnostics.record_detail("wgpu_driver", &adapter.driver);
                app_diagnostics.record_detail("wgpu_driver_info", &adapter.driver_info);
            }
            let app =
                RemoteOpsAgentApp::new(cc, startup, requested_language, app_diagnostics.clone());
            app_diagnostics.record("app_created");
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => {
            diagnostics.record("native_window_closed");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            diagnostics.record_native_window_error(&error);
            show_native_startup_error(renderer, &error, diagnostics);
            std::process::ExitCode::FAILURE
        }
    }
}

/// 在独立 Windows UI 线程运行窗口，确保窗口 TLS 在线程退出时提前释放。
#[cfg(target_os = "windows")]
fn launch_gui(args: Args, diagnostics: &StartupDiagnostics) -> std::process::ExitCode {
    let thread_diagnostics = diagnostics.clone();
    let gui_thread = std::thread::Builder::new()
        .name("remoteops-agent-gui-ui".to_owned())
        .spawn(move || run_gui(args, &thread_diagnostics));
    let Ok(gui_thread) = gui_thread else {
        diagnostics.record("ui_thread_spawn_failed");
        return std::process::ExitCode::FAILURE;
    };
    if let Ok(exit_code) = gui_thread.join() {
        diagnostics.record("ui_thread_joined");
        exit_code
    } else {
        diagnostics.record("ui_thread_panicked");
        std::process::ExitCode::FAILURE
    }
}

/// 非 Windows 构建保持当前线程运行，避免改变其他平台的事件循环约束。
#[cfg(not(target_os = "windows"))]
fn launch_gui(args: Args, diagnostics: &StartupDiagnostics) -> std::process::ExitCode {
    run_gui(args, diagnostics)
}

fn main() -> std::process::ExitCode {
    install_panic_reporter();
    let diagnostics = StartupDiagnostics::start();
    diagnostics.record("panic_reporter_installed");
    let _ = rustls::crypto::ring::default_provider().install_default();
    diagnostics.record("rustls_provider_installed");
    let args = Args::parse();
    diagnostics.record("arguments_parsed");
    launch_gui(args, &diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_window_options_return_errors_from_dedicated_ui_thread() {
        let options = agent_native_options(default_renderer(), false);

        assert!(options.run_and_return);
        assert!(!options.persist_window);
        assert!(options.centered);
        assert_eq!(RUNNING_WINDOW_SIZE, Vec2::new(520.0, 410.0));
        assert_eq!(options.viewport.inner_size, Some(RUNNING_WINDOW_SIZE));
        assert_eq!(options.viewport.min_inner_size, Some(RUNNING_WINDOW_SIZE));
        assert_eq!(options.viewport.max_inner_size, Some(RUNNING_WINDOW_SIZE));
        assert_eq!(options.viewport.resizable, Some(false));
        assert_eq!(options.viewport.maximize_button, Some(false));
        #[cfg(target_os = "windows")]
        assert!(options.event_loop_builder.is_some());
    }

    #[test]
    fn pairing_code_row_has_bounded_height_in_running_window() {
        assert_eq!(pairing_code_row_size(321.0), Vec2::new(321.0, 40.0));
    }

    #[test]
    fn pairing_code_content_uses_explicit_centering_padding() {
        assert!((pairing_code_left_padding(440.0, 164.0, 8.0) - 116.0).abs() < f32::EPSILON);
        assert!(pairing_code_left_padding(180.0, 164.0, 8.0).abs() < f32::EPSILON);
    }

    #[test]
    fn pairing_code_display_and_countdown_are_stable() {
        assert_eq!(display_pairing_code("482-915-307"), "482-915-307");
        assert_eq!(display_pairing_code("pending"), "pending");
        assert_eq!(format_countdown(599), "09:59");
        assert_eq!(format_countdown(3_661), "1:01:01");

        let now = DateTime::parse_from_rfc3339("2026-08-25T10:00:00Z")
            .expect("测试时间应有效")
            .with_timezone(&Utc);
        let future = now + chrono::Duration::seconds(90);
        assert_eq!(pairing_code_remaining_seconds(&future, &now), Some(90));
        assert_eq!(pairing_code_remaining_seconds(&now, &now), None);
    }

    #[test]
    fn copy_button_visuals_are_stable_across_pointer_states() {
        let mut style = egui::Style {
            animation_time: 0.18,
            ..Default::default()
        };
        style.visuals.widgets.hovered.bg_fill = Color32::BLACK;
        style.visuals.widgets.active.bg_fill = Color32::WHITE;
        let inactive = style.visuals.widgets.inactive;

        stabilize_copy_button_style(&mut style);

        assert!(style.animation_time.abs() < f32::EPSILON);
        assert_eq!(style.visuals.widgets.hovered, inactive);
        assert_eq!(style.visuals.widgets.active, inactive);
    }

    #[test]
    fn lower_section_groups_use_explicit_centering_padding() {
        assert!((centered_left_padding(440.0, 320.0) - 60.0).abs() < f32::EPSILON);
        assert!((centered_left_padding(212.0, 116.0) - 48.0).abs() < f32::EPSILON);
        assert!(centered_left_padding(100.0, 116.0).abs() < f32::EPSILON);
    }

    #[test]
    fn setup_window_starts_centered_with_setup_dimensions() {
        let options = agent_native_options(default_renderer(), true);

        assert!(options.centered);
        assert_eq!(options.viewport.inner_size, Some(SETUP_WINDOW_SIZE));
        assert_eq!(options.viewport.min_inner_size, Some(SETUP_WINDOW_SIZE));
        assert_eq!(options.viewport.max_inner_size, Some(SETUP_WINDOW_SIZE));
        assert_eq!(options.viewport.resizable, Some(false));
        assert_eq!(options.viewport.maximize_button, Some(false));
    }

    #[test]
    fn centered_window_position_uses_stable_outer_size() {
        let monitor = Vec2::new(2_560.0, 1_440.0);
        let outer = Vec2::new(656.0, 499.0);

        assert!(window_size_is_stable(outer, Vec2::new(656.5, 498.5)));
        assert!(!window_size_is_stable(Vec2::new(716.0, 739.0), outer));
        assert_eq!(
            centered_window_position(monitor, outer),
            egui::pos2(952.0, 470.5)
        );
    }

    #[test]
    fn windows_defaults_to_wgpu_and_glow_remains_selectable() {
        if cfg!(target_os = "windows") {
            assert_eq!(default_renderer(), eframe::Renderer::Wgpu);
            let options = agent_native_options(default_renderer(), false);
            assert_eq!(
                options.wgpu_options.surface,
                eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT
            );
        }
        assert_eq!(
            Args::try_parse_from(["remoteops-agent-gui", "--renderer", "glow"])
                .expect("glow 应保持为可选诊断渲染器")
                .renderer,
            eframe::Renderer::Glow
        );
    }

    #[test]
    fn native_window_error_report_keeps_display_and_debug_details() {
        let error =
            eframe::Error::AppCreation(Box::new(std::io::Error::other("native-window-test")));

        let report = format_native_window_error(&error);

        assert!(report.contains("error_display: app creation error: native-window-test"));
        assert!(report.contains("error_debug: AppCreation"));
    }

    #[test]
    fn explicit_missing_config_opens_setup_with_clear_error() {
        let missing = std::env::temp_dir().join(format!(
            "remoteops-missing-agent-config-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        let args = Args::try_parse_from([
            "remoteops-agent-gui".into(),
            "--config".into(),
            missing.clone().into_os_string(),
        ])
        .expect("显式配置参数应可解析");
        let StartupState::Setup(setup) = args.into_startup() else {
            panic!("不存在的显式配置应进入首次设置界面");
        };
        assert_eq!(setup.config_path, missing);
        assert!(setup.explicit_config);
        assert!(
            setup
                .error
                .as_deref()
                .is_some_and(|error| error.contains("配置文件不存在"))
        );
    }

    #[test]
    fn setup_error_message_shortens_missing_config_and_flattens_lines() {
        assert_eq!(
            setup_error_message("Agent 配置文件不存在：D:\\remoteops\\agent-config.json"),
            "Agent 配置文件不存在，请确认上方路径后重试。"
        );
        assert_eq!(setup_error_message("第一行\n第二行"), "第一行 第二行");
    }

    #[test]
    fn executable_logs_directory_has_highest_priority() {
        let executable = std::env::current_exe().expect("测试进程应有可执行文件路径");
        let expected = executable
            .parent()
            .expect("测试进程路径应有父目录")
            .join("logs");

        assert_eq!(diagnostic_directories().first(), Some(&expected));
    }

    #[test]
    fn setup_form_builds_valid_config_and_infers_server_name() {
        let mut setup = AgentSetupForm::new(
            AgentConfig::default(),
            PathBuf::from("agent-config.json"),
            false,
            None,
        );
        setup.relay = " relay.example.com:7443 ".to_owned();

        let config = setup.build_config().expect("首次设置应生成有效配置");

        assert_eq!(config.relay, "relay.example.com:7443");
        assert_eq!(config.server_name, "relay.example.com");
        assert_eq!(config.retry_seconds, AgentConfig::default().retry_seconds);
    }

    #[test]
    fn configured_full_access_is_shared_with_agent_runtime() {
        let owner_id = remoteops_domain::ControllerOwnerId::new();
        let mut config = AgentConfig::default();
        config.session_full_access_owners.insert(owner_id);

        let permission_control = AgentPermissionControl::from_config(&config);

        assert_eq!(
            permission_control.permission_mode(),
            remoteops_domain::PermissionMode::FullAccess
        );
    }

    #[test]
    fn default_config_save_falls_back_to_legacy_path() {
        let requested = default_agent_config_path();
        let legacy = legacy_agent_config_path();
        let mut attempts = Vec::new();

        let saved = persist_agent_config_with(&requested, true, |path| {
            attempts.push(path.to_path_buf());
            if path == requested {
                anyhow::bail!("模拟便携目录不可写");
            }
            Ok(())
        })
        .expect("默认配置应回退到旧版用户目录");

        assert_eq!(saved, legacy);
        assert_eq!(attempts, vec![requested, legacy]);
    }

    #[test]
    fn controller_status_reports_one_owner() {
        let (event_sender, events) = mpsc::channel();
        let (trust_sender, trust_events) = mpsc::channel();
        let (shutdown, _receiver) = watch::channel(false);
        let app = RemoteOpsAgentApp {
            diagnostics: StartupDiagnostics { path: None },
            event_sender,
            events,
            trust_sender,
            trust_events,
            shutdown,
            worker: None,
            trust_worker: None,
            setup: None,
            config_path: PathBuf::from("agent-config.json"),
            certificate_confirmation: None,
            status: UiStatus::Waiting,
            agent_instance_id: None,
            relay: "relay.example.com:7443".to_owned(),
            transfer_root: None,
            capabilities: CapabilitySet::default(),
            pairing_code: None,
            pairing_code_expires_at: None,
            active_connections: 1,
            controller_bindings: Vec::new(),
            permission_control: AgentPermissionControl::default(),
            elevated: Some(true),
            translator: Translator::new(remoteops_i18n::Language::ZhCn),
            language_file: None,
            show_stop_confirmation: false,
            show_advanced_settings: false,
            allow_close: false,
            copied_until: None,
            layout_is_setup: None,
            window_centering: WindowCenteringState::default(),
        };
        assert_eq!(app.controller_status_label(), "工程师已连接");
    }
}
