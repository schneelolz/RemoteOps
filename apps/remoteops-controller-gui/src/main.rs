#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai_settings;
mod backend;
mod serial_workbench;
mod theme;
mod timeline;
mod widgets;

use std::{collections::VecDeque, path::PathBuf, time::Duration};

use ai_settings::{AiProtocol, AiSettings, AiSettingsStore, delete_all};
use backend::{
    AiClientConfig, BackendCommand, BackendEvent, BackendHandle, LiveConfig, PendingApproval,
    spawn_demo, spawn_live,
};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, ValueEnum};
use eframe::egui::{
    self, Align, Align2, Area, Color32, CursorIcon, FontId, Frame, Layout, Margin, Modifiers,
    Order, Panel, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2,
};
use egui_phosphor::regular as icons;
use remoteops_audit::default_audit_log_path;
use remoteops_domain::{
    ConnectionDescriptor, ConnectionState, ControllerOwnerId, EventPayload, EventSource,
    PairingCode, PermissionMode, RemoteEvent, RemoteOperation, RequestId, SessionId,
};
use remoteops_i18n::{Language, Translator};
use serde::{Deserialize, Serialize};
use serial_workbench::SerialWorkbench;
use theme::{
    Palette, ThemeMode, apply_theme, configure_fonts, current_palette, install_design_style,
};
use timeline::{RequestTerminal, RequestTimelineItem, TimelineItem, aggregate_events};
use widgets::{ButtonKind, card, icon, icon_button, styled_button};

/// `RemoteOps` 原生 GUI 的命令行参数。
#[derive(Debug, Parser)]
#[command(version, about = "RemoteOps Windows-first 人工控制端 GUI")]
struct Args {
    /// Relay TLS 地址。
    #[arg(long, env = "REMOTEOPS_RELAY")]
    relay: Option<String>,
    /// Relay 证书中的服务名或 IP。
    #[arg(long, env = "REMOTEOPS_SERVER_NAME")]
    server_name: Option<String>,
    /// Relay 自签名 CA 证书。
    #[arg(long, env = "REMOTEOPS_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// 人工 Controller Token；不写入 GUI 配置文件。
    #[arg(long, env = "REMOTEOPS_HUMAN_CONTROLLER_TOKEN", hide_env_values = true)]
    controller_token: Option<String>,
    /// Human 与 AI Controller 共同使用的稳定 Owner ID。
    #[arg(long, env = "REMOTEOPS_CONTROLLER_OWNER_ID")]
    owner_id: Option<ControllerOwnerId>,
    /// 首次配对或重新配对时请求的会话权限。
    #[arg(long, env = "REMOTEOPS_PERMISSION_MODE", value_enum, default_value_t = PermissionModeArg::ApprovalRequired)]
    permission_mode: PermissionModeArg,
    /// 本地脱敏审计文件。
    #[arg(long, env = "REMOTEOPS_AUDIT_LOG", default_value_os_t = default_audit_log_path())]
    audit_log: PathBuf,
    /// 使用本地虚构数据启动，不连接真实 Relay。
    #[arg(long)]
    demo: bool,
    /// Relay 断线后的自动重连秒数。
    #[arg(long, default_value_t = 2)]
    reconnect_seconds: u64,
    /// `OpenAI` 兼容聊天接口根地址；只从参数或环境变量读取。
    #[arg(long, env = "REMOTEOPS_AI_BASE_URL")]
    ai_base_url: Option<String>,
    /// AI API 密钥；不写入 GUI 配置文件。
    #[arg(long, env = "REMOTEOPS_AI_API_KEY", hide_env_values = true)]
    ai_api_key: Option<String>,
    /// AI 模型名称。
    #[arg(long, env = "REMOTEOPS_AI_MODEL", default_value = "gpt-4o-mini")]
    ai_model: String,
    /// 界面语言；优先级低于已保存的 GUI 设置。
    #[arg(long, env = "REMOTEOPS_LANG")]
    lang: Option<Language>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PermissionModeArg {
    ReadOnly,
    ApprovalRequired,
    FullAccess,
}

impl From<PermissionModeArg> for PermissionMode {
    fn from(value: PermissionModeArg) -> Self {
        match value {
            PermissionModeArg::ReadOnly => Self::ReadOnly,
            PermissionModeArg::ApprovalRequired => Self::ApprovalRequired,
            PermissionModeArg::FullAccess => Self::FullAccess,
        }
    }
}

/// 底部输入框当前发送目标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputMode {
    /// 将自然语言交给 AI，由 AI 通过只读工具查看远端。
    AskAi,
    /// 将输入内容作为明确的远程命令执行。
    DirectExecute,
}

impl InputMode {
    fn label(self, translator: &Translator) -> String {
        translator.text(match self {
            Self::AskAi => "controller.input.ask_ai",
            Self::DirectExecute => "controller.input.direct_execute",
        })
    }
}

/// 多行输入框的发送快捷键。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SendShortcut {
    /// 单按回车发送，Shift+回车换行。
    #[default]
    Enter,
    /// Ctrl+回车发送，单按回车换行。
    CtrlEnter,
}

impl SendShortcut {
    /// 返回菜单中显示的名称。
    fn label(self, translator: &Translator) -> String {
        translator.text(match self {
            Self::Enter => "controller.shortcut.enter",
            Self::CtrlEnter => "controller.shortcut.ctrl_enter",
        })
    }

    /// 返回输入框底部的快捷键提示。
    fn hint(self, translator: &Translator) -> String {
        translator.text(match self {
            Self::Enter => "controller.shortcut.enter_hint",
            Self::CtrlEnter => "controller.shortcut.ctrl_enter_hint",
        })
    }
}

/// 判断本帧的回车修饰键是否匹配当前发送设置。
fn send_shortcut_matches(shortcut: SendShortcut, modifiers: Modifiers) -> bool {
    match shortcut {
        SendShortcut::Enter => {
            !modifiers.ctrl && !modifiers.command && !modifiers.shift && !modifiers.alt
        }
        SendShortcut::CtrlEnter => {
            modifiers.ctrl && !modifiers.command && !modifiers.shift && !modifiers.alt
        }
    }
}

/// 计算时间线卡片内部可使用的高度，并为底部输入区和卡片内边距保留空间。
fn timeline_conversation_height(available_height: f32, compact: bool) -> f32 {
    let composer_reserved = if compact { 232.0 } else { 256.0 };
    let card_vertical_inset = 32.0;
    let minimum_height = if compact { 120.0 } else { 170.0 };
    (available_height - composer_reserved - card_vertical_inset).max(minimum_height)
}

/// 工程师对当前目标选择的审批模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApprovalMode {
    /// 仅允许只读诊断。
    ReadOnly,
    /// 修改远程状态前逐项确认。
    Confirm,
    /// 启动时已配置为会话级完全访问。
    FullAccess,
}

impl From<PermissionModeArg> for ApprovalMode {
    fn from(value: PermissionModeArg) -> Self {
        match value {
            PermissionModeArg::ReadOnly => Self::ReadOnly,
            PermissionModeArg::ApprovalRequired => Self::Confirm,
            PermissionModeArg::FullAccess => Self::FullAccess,
        }
    }
}

impl ApprovalMode {
    /// 返回模式名称。
    fn label(self, translator: &Translator) -> String {
        translator.text(match self {
            Self::ReadOnly => "controller.approval.read_only",
            Self::Confirm => "controller.approval.confirm",
            Self::FullAccess => "controller.approval.full_access",
        })
    }
}

/// 保存到本地 eframe 存储中的界面设置。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct GuiSettings {
    /// 用户选择的主题模式。
    theme_mode: ThemeMode,
    /// 用户选择的发送快捷键。
    #[serde(default)]
    send_shortcut: SendShortcut,
    /// 用户选择的界面语言。
    #[serde(default)]
    language: Option<Language>,
}

/// AI 在一次对话中执行的远程只读工具。
#[derive(Clone, Debug)]
struct AiToolRun {
    /// 对应的远程请求标识。
    request_id: RequestId,
    /// 实际执行的命令。
    command: String,
    /// Agent 返回的结果摘要；执行中时为空。
    summary: Option<String>,
    /// 远程进程退出码。
    exit_code: Option<i32>,
    /// 用户是否展开了原始工具详情。
    expanded: bool,
}

/// 一次 GUI 内置 AI 对话的当前状态。
#[derive(Clone, Debug)]
enum AiInteractionStatus {
    /// 已提交，等待聊天接口响应。
    Waiting,
    /// AI 正在通过受控工具查看远端。
    RunningTool,
    /// AI 已返回最终回答。
    Completed(String),
    /// AI 请求未能完成。
    Failed(String),
}

/// GUI 中一项可持续显示的 AI 对话。
#[derive(Clone, Debug)]
struct AiInteraction {
    /// 本地生成的对话标识。
    interaction_id: RequestId,
    /// AI 锁定的目标会话。
    session_id: SessionId,
    /// 用户提交的自然语言问题。
    prompt: String,
    /// 提交时间。
    occurred_at: chrono::DateTime<chrono::Local>,
    /// 当前执行状态。
    status: AiInteractionStatus,
    /// 本轮 AI 已经发起的远程只读工具调用。
    tools: Vec<AiToolRun>,
}

/// 时间线和 AI 对话合并后的渲染项。
#[derive(Clone, Debug)]
enum ConversationItem {
    /// 普通人工、系统或远程请求记录。
    Timeline(Box<TimelineItem>),
    /// 一轮 GUI 内置 AI 对话。
    Ai {
        /// AI 对话标识。
        interaction_id: RequestId,
        /// 用户提交问题的时间。
        occurred_at: DateTime<Utc>,
    },
}

impl ConversationItem {
    /// 返回用于稳定排序的发生时间。
    fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            Self::Timeline(timeline) => match timeline.as_ref() {
                TimelineItem::Event(event) => event.occurred_at,
                TimelineItem::Request(request) => request.occurred_at,
            },
            Self::Ai { occurred_at, .. } => *occurred_at,
        }
    }
}

/// 工程师控制端的界面状态。
#[allow(clippy::struct_excessive_bools)]
struct RemoteOpsApp {
    /// 与核心应用服务通信的后台句柄。
    backend: BackendHandle,
    /// 当前是否使用演示数据。
    demo: bool,
    /// Relay 或演示后端状态。
    status: String,
    /// Relay 当前是否在线。
    connected: bool,
    /// 可选的现场连接列表。
    connections: Vec<ConnectionDescriptor>,
    /// 当前锁定的不可变会话。
    selected_session: Option<SessionId>,
    /// 统一 Human、AI、System 事件流。
    events: VecDeque<RemoteEvent>,
    /// GUI 内置 AI 的问题、进度、回答和错误。
    ai_interactions: VecDeque<AiInteraction>,
    /// 当前启动配置是否包含可用的 AI 聊天接口。
    ai_configured: bool,
    /// AI 设置弹窗是否打开。
    ai_settings_open: bool,
    /// AI 设置弹窗中的非敏感配置。
    ai_settings_form: AiSettings,
    /// 用户本次手工输入的 Token；不会写入普通设置文件。
    ai_token_input: String,
    /// 从 Codex 导入、等待用户保存的 Token。
    pending_import_token: Option<String>,
    /// Windows 凭据管理器中是否已有 AI Token。
    ai_token_available: bool,
    /// AI 设置弹窗中的状态信息及成功标记。
    ai_settings_message: Option<(String, bool)>,
    /// 是否正在等待 AI 连接测试。
    ai_test_in_progress: bool,
    /// 等待人工处理的审批。
    approvals: Vec<PendingApproval>,
    /// 当前输入框内容。
    composer: String,
    /// 当前输入发送模式。
    input_mode: InputMode,
    /// 多行输入框当前使用的发送快捷键。
    send_shortcut: SendShortcut,
    /// 发送快捷键下拉菜单是否打开。
    send_menu_open: bool,
    /// 当前主题模式。
    theme_mode: ThemeMode,
    /// 共享界面翻译器。
    translator: Translator,
    /// 外部语言包路径。
    language_file: Option<PathBuf>,
    /// 后端启动时使用的实际会话权限。
    approval_mode: ApprovalMode,
    /// 当前是否由人工暂停 AI。
    ai_paused: bool,
    /// 主题菜单是否打开。
    theme_menu_open: bool,
    /// 添加连接弹窗是否打开。
    pairing_dialog_open: bool,
    /// 配对弹窗中的控制码。
    pairing_code: String,
    /// 配对弹窗中的客户名称。
    pairing_alias: String,
    /// 是否正在等待 Relay 完成配对。
    pairing_in_progress: bool,
    /// 配对字段附近显示的错误。
    pairing_error: Option<String>,
    /// 配对弹窗打开后是否需要聚焦控制码输入框。
    pairing_focus_requested: bool,
    /// 当前仍可取消的远程请求。
    active_request: Option<(SessionId, RequestId)>,
    /// 演示首屏是否已经设置过初始滚动位置。
    demo_scroll_initialized: bool,
    /// 临时提示及剩余显示时间。
    toast: Option<(String, f32)>,
    /// 独立串口工作台状态。
    serial_workbench: SerialWorkbench,
}

impl RemoteOpsApp {
    /// 创建 GUI 并选择真实 Relay 或演示后端。
    #[allow(clippy::too_many_lines)]
    fn new(cc: &eframe::CreationContext<'_>, mut args: Args) -> Self {
        configure_fonts(&cc.egui_ctx);
        let settings = cc
            .storage
            .and_then(|storage| eframe::get_value::<GuiSettings>(storage, "remoteops_gui_settings"))
            .unwrap_or_default();
        apply_theme(&cc.egui_ctx, settings.theme_mode);
        let language = settings
            .language
            .or(args.lang)
            .unwrap_or_else(Language::detect);
        let language_file = std::env::var_os("REMOTEOPS_LANG_FILE").map(PathBuf::from);
        let mut translator = Translator::new(language);
        if let Some(path) = &language_file {
            let _ = translator.overlay_file(path);
        }
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            translator.text("app.controller_title"),
        ));

        let has_live_config = args.relay.is_some()
            && args.server_name.is_some()
            && args.ca_cert.is_some()
            && args.controller_token.is_some()
            && args.owner_id.is_some()
            && !args.demo;
        let saved_ai_settings = AiSettingsStore::load().ok().flatten();
        let saved_ai_token = AiSettingsStore::read_token().ok().flatten();
        let cli_base_url = args.ai_base_url.take();
        let cli_api_key = args.ai_api_key.take();
        let ai_settings_form = cli_base_url.as_ref().map_or_else(
            || saved_ai_settings.unwrap_or_default(),
            |base_url| AiSettings {
                base_url: base_url.clone(),
                model: args.ai_model.clone(),
                protocol: AiProtocol::Auto,
            },
        );
        let active_ai_token = cli_api_key.or(saved_ai_token);
        let ai_token_available = active_ai_token.is_some();
        let ai = active_ai_token.and_then(|api_key| {
            ai_settings_form
                .normalized()
                .ok()
                .map(|settings| AiClientConfig {
                    base_url: settings.base_url,
                    api_key,
                    model: settings.model,
                    protocol: settings.protocol,
                })
        });
        let ai_configured = ai.is_some();
        let approval_mode = ApprovalMode::from(args.permission_mode);
        let backend = if has_live_config {
            spawn_live(LiveConfig {
                relay_address: args.relay.expect("已检查 Relay 地址"),
                server_name: args.server_name.expect("已检查 Relay 服务名"),
                ca_certificate: args.ca_cert.expect("已检查 CA 证书"),
                controller_token: args.controller_token.expect("已检查人工 Token"),
                owner_id: args.owner_id.expect("已检查 Controller Owner ID"),
                permission_mode: args.permission_mode.into(),
                audit_log: args.audit_log,
                reconnect_seconds: args.reconnect_seconds,
                ai,
            })
        } else {
            spawn_demo()
        };

        Self {
            backend,
            demo: !has_live_config,
            status: if has_live_config {
                translator.text("status.connecting")
            } else {
                translator.text("controller.team.demo")
            },
            connected: false,
            connections: Vec::new(),
            selected_session: None,
            events: VecDeque::with_capacity(300),
            ai_interactions: VecDeque::with_capacity(40),
            ai_configured,
            ai_settings_open: false,
            ai_settings_form,
            ai_token_input: String::new(),
            pending_import_token: None,
            ai_token_available,
            ai_settings_message: None,
            ai_test_in_progress: false,
            approvals: Vec::new(),
            composer: String::new(),
            input_mode: InputMode::AskAi,
            send_shortcut: settings.send_shortcut,
            send_menu_open: false,
            theme_mode: settings.theme_mode,
            translator,
            language_file,
            approval_mode,
            ai_paused: false,
            theme_menu_open: false,
            pairing_dialog_open: false,
            pairing_code: String::new(),
            pairing_alias: String::new(),
            pairing_in_progress: false,
            pairing_error: None,
            pairing_focus_requested: false,
            active_request: None,
            demo_scroll_initialized: false,
            toast: None,
            serial_workbench: SerialWorkbench::default(),
        }
    }

    /// 切换界面语言，并重新应用可选外部语言包。
    fn set_language(&mut self, language: Language, ctx: &egui::Context) {
        self.translator.set_language(language);
        if let Some(path) = &self.language_file {
            let _ = self.translator.overlay_file(path);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            self.translator.text("app.controller_title"),
        ));
    }

    /// 处理异步后端发给 GUI 的全部待消费通知。
    fn process_backend_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.backend.events.try_recv() {
            self.apply_backend_event(event);
        }
        ctx.request_repaint_after(Duration::from_millis(120));
    }

    /// 把一条后端通知合并到界面状态。
    #[allow(clippy::too_many_lines)]
    fn apply_backend_event(&mut self, event: BackendEvent) {
        self.serial_workbench.apply_event(&event, &self.translator);
        match event {
            BackendEvent::Ready { demo, message } => {
                self.demo = demo;
                self.status = message;
            }
            BackendEvent::Connected(connected) => {
                self.connected = connected;
                if !self.demo {
                    self.status = if connected {
                        self.translator.text("controller.status.connected")
                    } else {
                        self.translator.text("controller.status.reconnecting")
                    };
                }
                if !connected && self.pairing_in_progress {
                    self.pairing_in_progress = false;
                    self.pairing_error = Some(
                        self.translator
                            .text("controller.status.pairing_disconnected"),
                    );
                }
            }
            BackendEvent::Connections(connections) => {
                if self.selected_session.is_none() {
                    self.selected_session =
                        connections.first().map(|connection| connection.session_id);
                }
                if self.selected_session.is_some_and(|session_id| {
                    !connections.iter().any(|item| item.session_id == session_id)
                }) {
                    self.selected_session =
                        connections.first().map(|connection| connection.session_id);
                }
                self.connections = connections;
            }
            BackendEvent::PairingCompleted {
                session_id,
                message,
            } => {
                self.selected_session = Some(session_id);
                self.pairing_in_progress = false;
                self.pairing_dialog_open = false;
                self.pairing_code.clear();
                self.pairing_alias.clear();
                self.pairing_error = None;
                self.set_toast(message);
            }
            BackendEvent::PairingFailed(error) => {
                self.pairing_in_progress = false;
                self.pairing_error = Some(error);
            }
            BackendEvent::RemoteEvent(event) => {
                if let Some(request_id) = event.request_id {
                    match event.payload {
                        EventPayload::OperationRequested { .. } => {
                            self.active_request = Some((event.session_id, request_id));
                        }
                        EventPayload::OperationCompleted { .. }
                        | EventPayload::OperationFailed { .. }
                        | EventPayload::OperationCancelled
                            if self.active_request == Some((event.session_id, request_id)) =>
                        {
                            self.active_request = None;
                        }
                        _ => {}
                    }
                }
                if self.events.len() >= 300 {
                    self.events.pop_front();
                }
                self.events.push_back(event);
            }
            BackendEvent::Approval(approval) => {
                self.serial_workbench.set_approval(approval.clone());
                if !self
                    .approvals
                    .iter()
                    .any(|item| item.approval_id == approval.approval_id)
                {
                    self.approvals.push(approval);
                }
            }
            BackendEvent::Message(message) => self.set_toast(message),
            BackendEvent::AiToolStarted {
                interaction_id,
                request_id,
                command,
            } => self.start_ai_tool(interaction_id, request_id, command),
            BackendEvent::AiToolCompleted {
                interaction_id,
                request_id,
                command,
                summary,
                exit_code,
            } => self.complete_ai_tool(interaction_id, request_id, command, summary, exit_code),
            BackendEvent::AiAnswer {
                interaction_id,
                text,
            } => self.update_ai_interaction(interaction_id, AiInteractionStatus::Completed(text)),
            BackendEvent::AiFailed {
                interaction_id,
                message,
            } => self.update_ai_interaction(interaction_id, AiInteractionStatus::Failed(message)),
            BackendEvent::AiConfigUpdated {
                configured,
                message,
            } => {
                self.ai_configured = configured;
                self.ai_token_available = configured;
                self.ai_token_input.clear();
                self.pending_import_token = None;
                self.ai_settings_message = Some((message.clone(), true));
                self.set_toast(message);
            }
            BackendEvent::AiConnectionTested { success, message } => {
                self.ai_test_in_progress = false;
                self.ai_settings_message = Some((message, success));
            }
            BackendEvent::Error(error) => {
                self.status.clone_from(&error);
                self.set_toast(error);
            }
            BackendEvent::SerialPorts { .. }
            | BackendEvent::SerialOpened { .. }
            | BackendEvent::SerialClosed { .. }
            | BackendEvent::SerialWriteCompleted { .. }
            | BackendEvent::SerialStatus { .. }
            | BackendEvent::SerialAiAnswer { .. }
            | BackendEvent::SerialAiFailed { .. } => {}
        }
    }

    /// 返回当前锁定的连接。
    fn selected_connection(&self) -> Option<&ConnectionDescriptor> {
        self.selected_session.and_then(|session_id| {
            self.connections
                .iter()
                .find(|item| item.session_id == session_id)
        })
    }

    /// 设置右下角临时提示。
    fn set_toast(&mut self, message: String) {
        self.toast = Some((message, 3.2));
    }

    /// 更新指定 AI 对话的进度��并保持错误信息不会自动消失。
    fn update_ai_interaction(&mut self, interaction_id: RequestId, status: AiInteractionStatus) {
        if let Some(interaction) = self
            .ai_interactions
            .iter_mut()
            .find(|item| item.interaction_id == interaction_id)
        {
            interaction.status = status;
        }
    }

    /// 记录 AI 发起的远程工具请求，并把对话切换到执行中状态。
    fn start_ai_tool(&mut self, interaction_id: RequestId, request_id: RequestId, command: String) {
        if let Some(interaction) = self
            .ai_interactions
            .iter_mut()
            .find(|item| item.interaction_id == interaction_id)
        {
            interaction.status = AiInteractionStatus::RunningTool;
            if !interaction
                .tools
                .iter()
                .any(|tool| tool.request_id == request_id)
            {
                interaction.tools.push(AiToolRun {
                    request_id,
                    command,
                    summary: None,
                    exit_code: None,
                    expanded: false,
                });
            }
        }
    }

    /// 合并 AI 远程工具的最终输出，供聊天气泡按需展开。
    fn complete_ai_tool(
        &mut self,
        interaction_id: RequestId,
        request_id: RequestId,
        command: String,
        summary: String,
        exit_code: Option<i32>,
    ) {
        if let Some(interaction) = self
            .ai_interactions
            .iter_mut()
            .find(|item| item.interaction_id == interaction_id)
        {
            let tool = if let Some(tool) = interaction
                .tools
                .iter_mut()
                .find(|tool| tool.request_id == request_id)
            {
                tool
            } else {
                interaction.tools.push(AiToolRun {
                    request_id,
                    command,
                    summary: None,
                    exit_code: None,
                    expanded: false,
                });
                interaction.tools.last_mut().expect("刚插入的工具必须存在")
            };
            tool.summary = Some(summary);
            tool.exit_code = exit_code;
        }
    }

    /// 判断某个远程请求是否属于 GUI 内置 AI 的工具调用。
    fn is_ai_tool_request(&self, request_id: RequestId) -> bool {
        self.ai_interactions.iter().any(|interaction| {
            interaction
                .tools
                .iter()
                .any(|tool| tool.request_id == request_id)
        })
    }

    /// 切换 AI 工具原始输出的展开状态。
    fn toggle_ai_tool_details(&mut self, request_id: RequestId) {
        if let Some(tool) = self
            .ai_interactions
            .iter_mut()
            .flat_map(|interaction| interaction.tools.iter_mut())
            .find(|tool| tool.request_id == request_id)
        {
            tool.expanded = !tool.expanded;
        }
    }

    /// 向后端线程发送一项用户操作。
    fn send(&mut self, command: BackendCommand) {
        if let Err(error) = self.backend.commands.send(command) {
            self.set_toast(self.translator.text_with(
                "controller.backend.unresponsive",
                &[("error", &error.to_string())],
            ));
        }
    }

    /// 渲染左侧连接和主题区域。
    #[allow(clippy::too_many_lines)]
    fn render_sidebar(&mut self, ui: &mut Ui, palette: Palette) {
        let ctx = ui.ctx().clone();
        let mut theme_anchor = None;
        Panel::left("remoteops_sidebar")
            .resizable(false)
            .default_size(286.0)
            .size_range(286.0..=286.0)
            .frame(
                Frame::new()
                    .fill(palette.surface)
                    .stroke(Stroke::new(1.0, palette.line))
                    .inner_margin(Margin::symmetric(18, 22)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    Frame::new()
                        .fill(palette.blue)
                        .corner_radius(12.0)
                        .inner_margin(Margin::same(9))
                        .shadow(palette.shadow_small)
                        .show(ui, |ui| {
                            ui.label(icon(icons::SHIELD_CHECK, 24.0, Color32::WHITE));
                        });
                    ui.add_space(3.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("RemoteOps").size(20.0).strong());
                        ui.label(
                            RichText::new(self.translator.text("controller.brand.control_center"))
                                .size(13.0)
                                .color(palette.text_muted),
                        );
                    });
                });

                ui.add_space(28.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(self.translator.text("controller.section.connections"))
                            .size(16.0)
                            .strong(),
                    );
                    ui.label(icon(
                        icons::CIRCLE,
                        9.0,
                        if self.connected {
                            palette.green
                        } else {
                            palette.text_faint
                        },
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let add_button = ui
                            .add_enabled_ui(!self.demo, |ui| {
                                styled_button(
                                    ui,
                                    icon(icons::PLUS, 18.0, palette.text_muted),
                                    ButtonKind::Ghost,
                                    palette,
                                    Vec2::splat(44.0),
                                )
                            })
                            .inner;
                        if add_button
                            .on_hover_text(if self.demo {
                                self.translator
                                    .text("controller.connection.add_demo_disabled")
                            } else if !self.connected {
                                self.translator.text("controller.connection.add_waiting")
                            } else {
                                self.translator.text("controller.connection.add")
                            })
                            .clicked()
                        {
                            self.open_pairing_dialog();
                        }
                    });
                });
                ui.add_space(10.0);

                if self.connections.is_empty() {
                    card(
                        palette,
                        palette.surface_muted,
                        palette.line,
                        8,
                        Margin::symmetric(12, 16),
                    )
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.vertical_centered(|ui| {
                            ui.label(icon(icons::MONITOR, 22.0, palette.text_faint));
                            ui.add_space(5.0);
                            ui.label(
                                RichText::new(self.translator.text("controller.connection.empty"))
                                    .size(13.0)
                                    .color(palette.text_muted),
                            );
                        });
                    });
                    ui.add_space(8.0);
                }

                for connection in self.connections.clone() {
                    let selected = self.selected_session == Some(connection.session_id);
                    let fill = if selected {
                        palette.blue_soft
                    } else {
                        Color32::TRANSPARENT
                    };
                    let stroke = if selected {
                        palette.blue_border
                    } else {
                        Color32::TRANSPARENT
                    };
                    let response = Frame::new()
                        .fill(fill)
                        .stroke(Stroke::new(1.0, stroke))
                        .corner_radius(16.0)
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_min_height(62.0);
                            ui.horizontal(|ui| {
                                Frame::new()
                                    .fill(palette.surface)
                                    .stroke(Stroke::new(1.0, palette.line))
                                    .corner_radius(14.0)
                                    .inner_margin(Margin::same(10))
                                    .show(ui, |ui| {
                                        ui.label(icon(
                                            icons::MONITOR,
                                            22.0,
                                            if selected {
                                                palette.blue
                                            } else {
                                                palette.text_muted
                                            },
                                        ));
                                    });
                                ui.add_space(2.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(
                                            connection
                                                .alias
                                                .as_deref()
                                                .unwrap_or(&connection.hostname),
                                        )
                                        .size(14.0)
                                        .strong(),
                                    );
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(icon(icons::CIRCLE, 8.0, palette.green));
                                        ui.label(
                                            RichText::new(connection_state_label(
                                                connection.state,
                                                &self.translator,
                                            ))
                                            .size(12.0)
                                            .color(palette.text_muted),
                                        );
                                    });
                                });
                                if selected {
                                    ui.with_layout(Layout::right_to_left(Align::BOTTOM), |ui| {
                                        ui.label(
                                            RichText::new(self.translator.text_with(
                                                "controller.connection.ai_target",
                                                &[("icon", icons::LOCK_SIMPLE)],
                                            ))
                                            .size(11.0)
                                            .strong()
                                            .color(palette.blue),
                                        );
                                    });
                                }
                            });
                        })
                        .response
                        .interact(Sense::click())
                        .on_hover_text(self.translator.text("controller.connection.switch_hint"));
                    let mut select_from_menu = false;
                    let mut disconnect_from_menu = false;
                    response.context_menu(|ui| {
                        ui.set_min_width(168.0);
                        if ui
                            .button(RichText::new(format!(
                                "{}  {}",
                                icons::TARGET,
                                self.translator.text("controller.connection.set_current")
                            )))
                            .clicked()
                        {
                            select_from_menu = true;
                            ui.close();
                        }
                        ui.separator();
                        let disconnect = ui.add_enabled(
                            !self.demo && connection.state != ConnectionState::Closed,
                            egui::Button::new(
                                RichText::new(format!(
                                    "{}  {}",
                                    icons::X_CIRCLE,
                                    self.translator.text("controller.connection.disconnect")
                                ))
                                .color(palette.red),
                            ),
                        );
                        if disconnect.clicked() {
                            disconnect_from_menu = true;
                            ui.close();
                        }
                    });
                    if disconnect_from_menu {
                        self.send(BackendCommand::Disconnect {
                            session_id: connection.session_id,
                        });
                        self.set_toast(format!(
                            "{}{}…",
                            self.translator.text("controller.connection.disconnecting"),
                            connection.alias.as_deref().unwrap_or(&connection.hostname)
                        ));
                    } else if response.clicked() || select_from_menu {
                        self.selected_session = Some(connection.session_id);
                        self.set_toast(format!(
                            "{}{}",
                            self.translator.text("controller.connection.selected"),
                            connection.alias.as_deref().unwrap_or(&connection.hostname)
                        ));
                    }
                    ui.add_space(8.0);
                }

                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let current_language = self.translator.language();
                    let next_language = match current_language {
                        Language::ZhCn => Language::EnUs,
                        Language::EnUs => Language::ZhCn,
                    };
                    if styled_button(
                        ui,
                        RichText::new(format!(
                            "{}  {}",
                            icons::TRANSLATE,
                            self.translator
                                .text(&format!("language.{}", current_language.code()))
                        ))
                        .size(12.0)
                        .color(palette.text_muted),
                        ButtonKind::Ghost,
                        palette,
                        Vec2::new(ui.available_width(), 42.0),
                    )
                    .on_hover_text(self.translator.text("language.switch_hint"))
                    .clicked()
                    {
                        self.set_language(next_language, &ctx);
                    }

                    ui.add_space(6.0);
                    let theme_button = styled_button(
                        ui,
                        RichText::new(self.translator.text_with(
                            "controller.theme.title",
                            &[
                                (
                                    "theme",
                                    &theme_mode_label(self.theme_mode, &self.translator),
                                ),
                                ("caret", icons::CARET_DOWN),
                            ],
                        ))
                        .size(12.0)
                        .color(palette.text_muted),
                        ButtonKind::Ghost,
                        palette,
                        Vec2::new(ui.available_width(), 42.0),
                    );
                    if theme_button.clicked() {
                        self.theme_menu_open = !self.theme_menu_open;
                    }
                    theme_anchor = Some(theme_button.rect);

                    ui.add_space(6.0);
                    let ai_state = if self.ai_configured {
                        self.translator.text("controller.ai.configured")
                    } else {
                        self.translator.text("controller.ai.not_configured")
                    };
                    if styled_button(
                        ui,
                        RichText::new(format!(
                            "{}  {} · {}",
                            icons::SPARKLE,
                            self.translator.text("controller.ai.settings"),
                            ai_state
                        ))
                        .size(12.0)
                        .color(if self.ai_configured {
                            palette.green
                        } else {
                            palette.text_muted
                        }),
                        ButtonKind::Ghost,
                        palette,
                        Vec2::new(ui.available_width(), 42.0),
                    )
                    .clicked()
                    {
                        self.open_ai_settings();
                    }

                    ui.add_space(6.0);
                    let serial_button = ui
                        .add_enabled_ui(self.selected_session.is_some(), |ui| {
                            styled_button(
                                ui,
                                RichText::new(format!(
                                    "{}  {}",
                                    icons::PLUGS_CONNECTED,
                                    self.translator.text("controller.serial.workbench")
                                ))
                                .size(12.0)
                                .color(palette.text_muted),
                                ButtonKind::Ghost,
                                palette,
                                Vec2::new(ui.available_width(), 42.0),
                            )
                        })
                        .inner;
                    if serial_button.clicked()
                        && let Some(session_id) = self.selected_session
                    {
                        if let Some(command) = self.serial_workbench.open_for(session_id) {
                            self.send(command);
                        }
                        self.send(BackendCommand::ListSerial { session_id });
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        Frame::new()
                            .fill(palette.blue)
                            .corner_radius(20.0)
                            .inner_margin(Margin::same(9))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("RO")
                                        .size(11.0)
                                        .strong()
                                        .color(Color32::WHITE),
                                );
                            });
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(self.translator.text("controller.team"))
                                    .size(13.0)
                                    .strong(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(icon(icons::CIRCLE, 8.0, palette.green));
                                ui.label(
                                    RichText::new(if self.demo {
                                        self.translator.text("controller.team.demo")
                                    } else if self.connected {
                                        self.translator.text("controller.team.online")
                                    } else {
                                        self.translator.text("controller.team.reconnecting")
                                    })
                                    .size(12.0)
                                    .color(palette.text_muted),
                                );
                            });
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(icon(icons::CARET_DOWN, 14.0, palette.text_faint));
                        });
                    });
                });
            });

        if self.theme_menu_open
            && let Some(anchor) = theme_anchor
        {
            self.render_theme_menu(&ctx, palette, anchor);
        }
    }

    /// 渲染侧栏底部的主题菜单。
    fn render_theme_menu(&mut self, ctx: &egui::Context, palette: Palette, anchor: egui::Rect) {
        let position = anchor.left_top() - Vec2::new(0.0, 160.0);
        Area::new("remoteops_theme_menu".into())
            .order(Order::Tooltip)
            .fixed_pos(position)
            .show(ctx, |ui| {
                ui.set_width(anchor.width());
                card(
                    palette,
                    palette.surface_raised,
                    palette.line,
                    14,
                    Margin::same(8),
                )
                .show(ui, |ui| {
                    for (mode, item_icon) in [
                        (ThemeMode::System, icons::DESKTOP),
                        (ThemeMode::Light, icons::SUN),
                        (ThemeMode::Dark, icons::MOON),
                    ] {
                        let selected = self.theme_mode == mode;
                        let response = Frame::new()
                            .fill(if selected {
                                palette.blue_soft
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(9.0)
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(icon(
                                        item_icon,
                                        17.0,
                                        if selected {
                                            palette.blue
                                        } else {
                                            palette.text_muted
                                        },
                                    ));
                                    ui.label(
                                        RichText::new(theme_mode_label(mode, &self.translator))
                                            .size(12.0)
                                            .color(if selected {
                                                palette.blue
                                            } else {
                                                palette.text_muted
                                            }),
                                    );
                                    if selected {
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.label(icon(icons::CHECK, 16.0, palette.blue));
                                            },
                                        );
                                    }
                                });
                            })
                            .response
                            .interact(Sense::click());
                        if response.clicked() {
                            self.theme_mode = mode;
                            self.theme_menu_open = false;
                            apply_theme(ctx, mode);
                            self.set_toast(self.translator.text_with(
                                "controller.theme.changed",
                                &[("theme", &theme_mode_label(mode, &self.translator))],
                            ));
                        }
                    }
                });
            });
    }

    /// 渲染当前目标、审批模式和暂停 AI 操作。
    #[allow(clippy::too_many_lines)]
    fn render_header(&mut self, ui: &mut Ui, palette: Palette) {
        card(
            palette,
            palette.surface,
            palette.line,
            18,
            Margin::symmetric(18, 13),
        )
        .show(ui, |ui| {
            ui.set_min_height(54.0);
            ui.horizontal(|ui| {
                Frame::new()
                    .fill(palette.surface_muted)
                    .stroke(Stroke::new(1.0, palette.line))
                    .corner_radius(15.0)
                    .inner_margin(Margin::same(11))
                    .show(ui, |ui| {
                        ui.label(icon(icons::TARGET, 24.0, palette.text_muted));
                    });
                ui.add_space(2.0);
                ui.vertical(|ui| {
                    if let Some(connection) = self.selected_connection() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(self.translator.text("controller.current_target"))
                                    .size(16.0)
                                    .color(palette.text_muted),
                            );
                            ui.label(
                                RichText::new(
                                    connection.alias.as_deref().unwrap_or(&connection.hostname),
                                )
                                .size(21.0)
                                .strong()
                                .color(palette.blue),
                            );
                        });
                        ui.label(
                            RichText::new(self.translator.text_with(
                                "controller.session_id",
                                &[("id", &short_session_id(connection.session_id))],
                            ))
                            .size(12.0)
                            .color(palette.text_muted),
                        );
                        let available_shells = connection
                            .environment
                            .shells
                            .iter()
                            .filter(|shell| shell.available)
                            .map(|shell| {
                                shell.version.as_ref().map_or_else(
                                    || format!("{:?}", shell.kind),
                                    |version| format!("{:?} {version}", shell.kind),
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" · ");
                        let elevated = match connection.environment.elevated {
                            Some(true) => self.translator.text("controller.boolean.yes"),
                            Some(false) => self.translator.text("controller.boolean.no"),
                            None => self.translator.text("controller.unknown"),
                        };
                        ui.label(
                            RichText::new(self.translator.text_with(
                                "controller.system_info",
                                &[
                                    ("system", &connection.operating_system),
                                    ("elevated", &elevated),
                                ],
                            ))
                            .size(12.0)
                            .color(palette.text_muted),
                        );
                        let available_shells = if available_shells.is_empty() {
                            self.translator.text("controller.shell.none")
                        } else {
                            available_shells
                        };
                        ui.label(
                            RichText::new(self.translator.text_with(
                                "controller.available_shells",
                                &[("shells", &available_shells)],
                            ))
                            .size(12.0)
                            .color(palette.text_muted),
                        );
                    } else {
                        ui.label(
                            RichText::new(self.translator.text("controller.no_target"))
                                .size(20.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(self.translator.text("controller.select_online"))
                                .size(12.0)
                                .color(palette.text_muted),
                        );
                    }
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let pause_kind = if self.ai_paused {
                        ButtonKind::Warning
                    } else {
                        ButtonKind::Secondary
                    };
                    let pause_icon = if self.ai_paused {
                        icons::ARROW_CLOCKWISE
                    } else {
                        icons::PAUSE_CIRCLE
                    };
                    let pause_label = if self.ai_paused {
                        self.translator.text("controller.resume_ai")
                    } else {
                        self.translator.text("controller.pause_ai")
                    };
                    if icon_button(
                        ui,
                        pause_icon,
                        &pause_label,
                        pause_kind,
                        palette,
                        Vec2::new(154.0, 44.0),
                    )
                    .clicked()
                        && let Some(session_id) = self.selected_session
                    {
                        if self.ai_paused {
                            self.send(BackendCommand::ReleaseTakeover { session_id });
                        } else {
                            self.send(BackendCommand::Takeover { session_id });
                        }
                        self.ai_paused = !self.ai_paused;
                    }

                    ui.add_sized(
                        [290.0, 44.0],
                        egui::Label::new(
                            RichText::new(format!(
                                "{}    {}",
                                self.translator.text("controller.approval.title"),
                                self.approval_mode.label(&self.translator)
                            ))
                            .size(13.0)
                            .color(palette.text_muted),
                        ),
                    );
                });
            });
        });
    }

    /// 渲染当前审批策略的一行状态提示。
    fn render_mode_note(&self, ui: &mut Ui, palette: Palette) {
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.add_space(17.0);
            let dot = match self.approval_mode {
                ApprovalMode::ReadOnly => palette.blue,
                ApprovalMode::Confirm => palette.green,
                ApprovalMode::FullAccess => palette.amber,
            };
            ui.label(icon(icons::CIRCLE, 8.0, dot));
            ui.label(
                RichText::new(self.translator.text(match self.approval_mode {
                    ApprovalMode::ReadOnly => "controller.approval.note.read_only",
                    ApprovalMode::Confirm => "controller.approval.note.confirm",
                    ApprovalMode::FullAccess => "controller.approval.note.full_access",
                }))
                .size(12.0)
                .color(palette.text_muted),
            );
        });
        ui.add_space(3.0);
    }

    /// 渲染协作记录、聚合后的终端输出和嵌入式审批卡片。
    #[allow(clippy::too_many_lines)]
    fn render_timeline(&mut self, ui: &mut Ui, palette: Palette) {
        let compact = ui.available_height() < 760.0;
        // 先为底部输入区预留完整空间，避免时间线的最小高度把输入区挤出工作区。
        // 同时扣除时间线卡片自身的上下内边距，避免非全屏窗口底部被额外撑出可视区。
        let conversation_height = timeline_conversation_height(ui.available_height(), compact);
        card(
            palette,
            palette.surface,
            palette.line,
            18,
            Margin::symmetric(18, 16),
        )
        .show(ui, |ui| {
            ui.set_min_height(conversation_height);
            ui.set_max_height(conversation_height);
            let mut timeline_scroll = ScrollArea::vertical()
                .id_salt("remoteops_timeline_v2")
                .auto_shrink([false, false])
                .stick_to_bottom(!self.demo)
                .max_height(conversation_height - 6.0);
            if self.demo && !self.demo_scroll_initialized && !self.events.is_empty() {
                timeline_scroll = timeline_scroll.vertical_scroll_offset(145.0);
            }
            timeline_scroll.show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let events: Vec<_> = self
                    .events
                    .iter()
                    .filter(|event| {
                        self.selected_session
                            .is_none_or(|id| event.session_id == id)
                    })
                    .cloned()
                    .collect();
                let mut conversation = aggregate_events(events)
                    .into_iter()
                    .filter(|item| {
                        !matches!(
                            item,
                            TimelineItem::Request(request)
                                if self.is_ai_tool_request(request.request_id)
                        )
                    })
                    .map(|item| ConversationItem::Timeline(Box::new(item)))
                    .collect::<Vec<_>>();
                conversation.extend(
                    self.ai_interactions
                        .iter()
                        .filter(|interaction| {
                            self.selected_session
                                .is_none_or(|id| interaction.session_id == id)
                        })
                        .map(|interaction| ConversationItem::Ai {
                            interaction_id: interaction.interaction_id,
                            occurred_at: interaction.occurred_at.with_timezone(&Utc),
                        }),
                );
                conversation.sort_by_key(ConversationItem::occurred_at);

                if conversation.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(self.translator.text("controller.timeline.empty"))
                                .color(palette.text_muted),
                        );
                    });
                }

                let mut embedded_approvals = Vec::new();
                for item in conversation {
                    match item {
                        ConversationItem::Timeline(timeline) => match timeline.as_ref() {
                            TimelineItem::Event(event) => {
                                render_event(ui, event, palette, &self.translator);
                            }
                            TimelineItem::Request(request) => {
                                if let Some((approval_id, _)) = request.approval.as_ref()
                                    && let Some(approval) = self
                                        .approvals
                                        .iter()
                                        .find(|item| item.approval_id == *approval_id)
                                        .cloned()
                                {
                                    render_approval(self, ui, &approval, palette);
                                    embedded_approvals.push(*approval_id);
                                } else {
                                    render_request_timeline_item(
                                        ui,
                                        request,
                                        palette,
                                        &self.translator,
                                    );
                                }
                            }
                        },
                        ConversationItem::Ai { interaction_id, .. } => {
                            let toggled = self
                                .ai_interactions
                                .iter()
                                .find(|interaction| interaction.interaction_id == interaction_id)
                                .map_or_else(Vec::new, |interaction| {
                                    render_ai_interaction(
                                        ui,
                                        interaction,
                                        palette,
                                        &self.translator,
                                    )
                                });
                            for request_id in toggled {
                                self.toggle_ai_tool_details(request_id);
                            }
                        }
                    }
                    ui.add_space(12.0);
                }

                let remaining: Vec<_> = self
                    .approvals
                    .iter()
                    .filter(|approval| {
                        self.selected_session
                            .is_none_or(|session_id| approval.session_id == session_id)
                            && !embedded_approvals.contains(&approval.approval_id)
                    })
                    .cloned()
                    .collect();
                for approval in remaining {
                    render_approval(self, ui, &approval, palette);
                    ui.add_space(16.0);
                }

                if let Some((session_id, request_id)) = self.active_request
                    && self.selected_session == Some(session_id)
                {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(
                            ui,
                            icons::STOP_CIRCLE,
                            &self.translator.text("controller.timeline.stop_task"),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::new(142.0, 40.0),
                        )
                        .clicked()
                        {
                            self.send(BackendCommand::Cancel {
                                session_id,
                                request_id,
                            });
                        }
                    });
                }
            });
            if self.demo && !self.demo_scroll_initialized && !self.events.is_empty() {
                self.demo_scroll_initialized = true;
            }
        });

        ui.add_space(12.0);
        self.render_composer(ui, palette);
        // 给输入卡片描边和阴影留下绘制空间，避免贴近窗口底部时被裁剪。
        ui.add_space(12.0);
    }

    /// 渲染底部大文本输入区和工具栏。
    #[allow(clippy::too_many_lines)]
    fn render_composer(&mut self, ui: &mut Ui, palette: Palette) {
        let mut send_clicked = false;
        let mut tool_message = None;
        let mut send_menu_anchor = None;
        let ctx = ui.ctx().clone();
        let composer_id = ui.make_persistent_id("remoteops_composer_input");
        let composer_focused = ui.memory(|memory| memory.has_focus(composer_id));
        let submit_shortcut = composer_focused
            && ctx.input_mut(|input| {
                if !send_shortcut_matches(self.send_shortcut, input.modifiers) {
                    return false;
                }
                input.consume_key(
                    match self.send_shortcut {
                        SendShortcut::Enter => Modifiers::NONE,
                        SendShortcut::CtrlEnter => Modifiers::CTRL,
                    },
                    egui::Key::Enter,
                )
            });
        let compact = ui.available_height() < 760.0;
        Frame::new()
            .fill(palette.surface)
            .stroke(Stroke::new(1.5, palette.blue))
            .corner_radius(18.0)
            .inner_margin(Margin::same(if compact { 12 } else { 18 }))
            .shadow(palette.shadow_small)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(self.translator.text("controller.composer.send_to"))
                            .size(12.0)
                            .color(palette.text_muted),
                    );
                    for mode in [InputMode::AskAi, InputMode::DirectExecute] {
                        let selected = self.input_mode == mode;
                        let kind = if selected {
                            ButtonKind::Secondary
                        } else {
                            ButtonKind::Ghost
                        };
                        if styled_button(
                            ui,
                            mode.label(&self.translator),
                            kind,
                            palette,
                            Vec2::new(96.0, if compact { 28.0 } else { 32.0 }),
                        )
                        .clicked()
                        {
                            self.input_mode = mode;
                        }
                    }
                    let helper = self.translator.text(match self.input_mode {
                        InputMode::AskAi if !self.ai_configured => "controller.composer.ai_missing",
                        InputMode::AskAi => "controller.composer.ai_helper",
                        InputMode::DirectExecute => "controller.composer.direct_helper",
                    });
                    ui.add(
                        egui::Label::new(RichText::new(helper).size(12.0).color(
                            if self.input_mode == InputMode::AskAi && !self.ai_configured {
                                palette.amber
                            } else {
                                palette.text_muted
                            },
                        ))
                        .wrap(),
                    );
                });
                ui.add_space(if compact { 5.0 } else { 8.0 });
                ui.add_sized(
                    [ui.available_width(), if compact { 76.0 } else { 92.0 }],
                    TextEdit::multiline(&mut self.composer)
                        .id(composer_id)
                        .desired_rows(if compact { 3 } else { 5 })
                        .desired_width(f32::INFINITY)
                        .frame(Frame::NONE)
                        .hint_text(self.translator.text(match self.input_mode {
                            InputMode::AskAi => "controller.composer.ai_placeholder",
                            InputMode::DirectExecute => "controller.composer.command_placeholder",
                        })),
                );
                ui.add_space(if compact { 4.0 } else { 5.0 });
                ui.horizontal(|ui| {
                    if compact {
                        ui.label(
                            RichText::new(self.send_shortcut.hint(&self.translator))
                                .size(11.0)
                                .color(palette.text_faint),
                        );
                    } else {
                        if icon_button(
                            ui,
                            icons::PLUS,
                            "",
                            ButtonKind::Ghost,
                            palette,
                            Vec2::new(38.0, 36.0),
                        )
                        .clicked()
                        {
                            tool_message = Some(
                                self.translator
                                    .text("controller.composer.choose_capability"),
                            );
                        }
                        if icon_button(
                            ui,
                            icons::FOLDER_OPEN,
                            &self.translator.text("controller.composer.file"),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::new(76.0, 36.0),
                        )
                        .clicked()
                        {
                            tool_message =
                                Some(self.translator.text("controller.composer.file_future"));
                        }
                        if icon_button(
                            ui,
                            icons::PLUGS_CONNECTED,
                            &self.translator.text("controller.composer.ssh_serial"),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::new(112.0, 36.0),
                        )
                        .clicked()
                        {
                            if let Some(session_id) = self.selected_session {
                                if let Some(command) = self.serial_workbench.open_for(session_id) {
                                    self.send(command);
                                }
                                self.send(BackendCommand::ListSerial { session_id });
                            } else {
                                tool_message =
                                    Some(self.translator.text("controller.composer.select_target"));
                            }
                        }
                        if icon_button(
                            ui,
                            icons::FILE_TEXT,
                            &self.translator.text("controller.composer.audit"),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::new(76.0, 36.0),
                        )
                        .clicked()
                        {
                            tool_message =
                                Some(self.translator.text("controller.composer.audit_future"));
                        }
                        ui.label(
                            RichText::new(self.send_shortcut.hint(&self.translator))
                                .size(11.0)
                                .color(palette.text_faint),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let menu_button = styled_button(
                                ui,
                                icon(icons::CARET_DOWN, 15.0, Color32::WHITE),
                                ButtonKind::Primary,
                                palette,
                                Vec2::new(40.0, if compact { 40.0 } else { 44.0 }),
                            );
                            let menu_rect = menu_button.rect;
                            if menu_button
                                .on_hover_text(
                                    self.translator.text("controller.composer.shortcut_menu"),
                                )
                                .clicked()
                            {
                                self.send_menu_open = !self.send_menu_open;
                            }
                            let send_button = icon_button(
                                ui,
                                icons::PAPER_PLANE_TILT,
                                &self.translator.text(match self.input_mode {
                                    InputMode::AskAi => "controller.composer.ask_ai",
                                    InputMode::DirectExecute => "controller.composer.execute",
                                }),
                                ButtonKind::Primary,
                                palette,
                                Vec2::new(118.0, if compact { 40.0 } else { 44.0 }),
                            );
                            if send_button.clicked() {
                                send_clicked = true;
                            }
                            send_menu_anchor = Some(send_button.rect.union(menu_rect));
                        });
                    });
                });
            });

        if self.send_menu_open
            && let Some(anchor) = send_menu_anchor
        {
            self.render_send_menu(&ctx, palette, anchor);
        }
        if send_clicked || submit_shortcut {
            self.send_menu_open = false;
            self.submit_input();
        } else if let Some(message) = tool_message {
            self.set_toast(message);
        }
    }

    /// 渲染发送按钮旁的快捷键选择菜单。
    fn render_send_menu(&mut self, ctx: &egui::Context, palette: Palette, anchor: egui::Rect) {
        let position = anchor.right_top() - Vec2::new(280.0, 132.0);
        Area::new("remoteops_send_shortcut_menu".into())
            .order(Order::Tooltip)
            .fixed_pos(position)
            .show(ctx, |ui| {
                ui.set_width(280.0);
                card(
                    palette,
                    palette.surface_raised,
                    palette.line_strong,
                    12,
                    Margin::same(8),
                )
                .show(ui, |ui| {
                    for shortcut in [SendShortcut::Enter, SendShortcut::CtrlEnter] {
                        let selected = self.send_shortcut == shortcut;
                        let response = Frame::new()
                            .fill(if selected {
                                palette.blue_soft
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(9.0)
                            .inner_margin(Margin::symmetric(10, 9))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(icon(
                                        if selected {
                                            icons::CHECK
                                        } else {
                                            icons::KEYBOARD
                                        },
                                        16.0,
                                        if selected {
                                            palette.blue
                                        } else {
                                            palette.text_faint
                                        },
                                    ));
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(shortcut.label(&self.translator))
                                                .size(12.5)
                                                .strong()
                                                .color(palette.text),
                                        );
                                        ui.label(
                                            RichText::new(self.translator.text(match shortcut {
                                                SendShortcut::Enter => {
                                                    "controller.shortcut.enter_insert"
                                                }
                                                SendShortcut::CtrlEnter => {
                                                    "controller.shortcut.ctrl_enter_insert"
                                                }
                                            }))
                                            .size(11.0)
                                            .color(palette.text_muted),
                                        );
                                    });
                                });
                            })
                            .response
                            .interact(Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand);
                        if response.clicked() {
                            self.send_shortcut = shortcut;
                            self.send_menu_open = false;
                            self.set_toast(format!(
                                "{}{}",
                                self.translator.text("controller.shortcut.changed_prefix"),
                                shortcut.label(&self.translator)
                            ));
                        }
                    }
                });
            });
    }

    /// 校验输入并按当前模式发送到 AI 或远程 Agent。
    fn submit_input(&mut self) {
        let input = self.composer.trim().to_owned();
        let Some(session_id) = self.selected_session else {
            self.set_toast(self.translator.text("controller.input.no_target"));
            return;
        };
        if input.is_empty() {
            self.set_toast(self.translator.text("controller.input.empty"));
            return;
        }
        match self.input_mode {
            InputMode::AskAi => {
                let interaction_id = RequestId::new();
                if self.ai_interactions.len() >= 40 {
                    self.ai_interactions.pop_front();
                }
                self.ai_interactions.push_back(AiInteraction {
                    interaction_id,
                    session_id,
                    prompt: input.clone(),
                    occurred_at: Local::now(),
                    status: AiInteractionStatus::Waiting,
                    tools: Vec::new(),
                });
                self.send(BackendCommand::AskAi {
                    session_id,
                    interaction_id,
                    prompt: input,
                });
            }
            InputMode::DirectExecute => {
                let readonly = self.approval_mode == ApprovalMode::ReadOnly;
                self.send(BackendCommand::RunCommand {
                    session_id,
                    command: input,
                    readonly,
                });
            }
        }
        if self.input_mode == InputMode::DirectExecute || self.ai_configured {
            self.composer.clear();
        }
    }

    /// 打开添加连接弹窗并重置上次输入状态。
    fn open_pairing_dialog(&mut self) {
        self.pairing_dialog_open = true;
        self.ai_settings_open = false;
        self.send_menu_open = false;
        self.pairing_in_progress = false;
        self.pairing_error = None;
        self.pairing_code.clear();
        self.pairing_alias.clear();
        self.pairing_focus_requested = true;
    }

    /// 打开 AI 设置弹窗并清理本次临时输入。
    fn open_ai_settings(&mut self) {
        self.ai_settings_open = true;
        self.pairing_dialog_open = false;
        self.send_menu_open = false;
        self.theme_menu_open = false;
        self.ai_token_input.clear();
        self.pending_import_token = None;
        self.ai_settings_message = None;
        self.ai_test_in_progress = false;
        if let Ok(Some(settings)) = AiSettingsStore::load() {
            self.ai_settings_form = settings;
        }
        self.ai_token_available = AiSettingsStore::read_token().ok().flatten().is_some();
    }

    /// 使用弹窗当前值构建完整 AI 客户端配置。
    fn pending_ai_client_config(&self) -> Result<AiClientConfig, String> {
        let settings = self
            .ai_settings_form
            .normalized()
            .map_err(|error| error.to_string())?;
        let api_key = if !self.ai_token_input.trim().is_empty() {
            self.ai_token_input.trim().to_owned()
        } else if let Some(token) = self.pending_import_token.as_deref() {
            token.to_owned()
        } else {
            AiSettingsStore::read_token()?
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| {
                    self.translator
                        .text("controller.ai_settings.token_required")
                })?
        };
        Ok(AiClientConfig {
            base_url: settings.base_url,
            api_key,
            model: settings.model,
            protocol: settings.protocol,
        })
    }

    /// 从本机 Codex 配置读取 Provider、模型、协议和 Token。
    fn import_codex_ai_settings(&mut self) {
        match AiSettingsStore::import_from_codex() {
            Ok(imported) => {
                self.ai_settings_form = imported.settings;
                self.ai_token_available =
                    self.ai_token_available || imported.bearer_token.is_some();
                self.pending_import_token = imported.bearer_token;
                self.ai_token_input.clear();
                self.ai_settings_message = Some((
                    self.translator.text("controller.ai_settings.imported"),
                    true,
                ));
            }
            Err(error) => {
                self.ai_settings_message = Some((error, false));
            }
        }
    }

    /// 测试弹窗中的候选 AI 配置。
    fn test_ai_settings(&mut self) {
        match self.pending_ai_client_config() {
            Ok(config) => {
                self.ai_test_in_progress = true;
                self.ai_settings_message =
                    Some((self.translator.text("controller.ai_settings.testing"), true));
                self.send(BackendCommand::TestAiConfig(config));
            }
            Err(error) => {
                self.ai_settings_message = Some((error, false));
            }
        }
    }

    /// 安全保存 AI 设置并立即更新后端客户端。
    fn save_ai_settings(&mut self) {
        match self.pending_ai_client_config() {
            Ok(config) => {
                let settings = AiSettings {
                    base_url: config.base_url.clone(),
                    model: config.model.clone(),
                    protocol: config.protocol,
                };
                match AiSettingsStore::save(&settings, &config.api_key) {
                    Ok(()) => {
                        self.ai_settings_form = settings;
                        self.ai_settings_message =
                            Some((self.translator.text("controller.ai_settings.saving"), true));
                        self.send(BackendCommand::UpdateAiConfig(Some(config)));
                    }
                    Err(error) => {
                        self.ai_settings_message = Some((error, false));
                    }
                }
            }
            Err(error) => {
                self.ai_settings_message = Some((error, false));
            }
        }
    }

    /// 删除 `RemoteOps` 自己保存的 AI 设置和凭据。
    fn clear_ai_settings(&mut self) {
        match delete_all().map_err(|error| error.to_string()) {
            Ok(()) => {
                self.ai_settings_form = AiSettings::default();
                self.ai_token_input.clear();
                self.pending_import_token = None;
                self.ai_token_available = false;
                self.ai_settings_message =
                    Some((self.translator.text("controller.ai_settings.deleted"), true));
                self.send(BackendCommand::UpdateAiConfig(None));
            }
            Err(error) => {
                self.ai_settings_message = Some((error, false));
            }
        }
    }

    /// 渲染应用内 AI 设置弹窗。
    #[allow(clippy::too_many_lines)]
    fn render_ai_settings_dialog(&mut self, ctx: &egui::Context, palette: Palette) {
        if !self.ai_settings_open {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) && !self.ai_test_in_progress {
            self.ai_settings_open = false;
            return;
        }

        let content_rect = ctx.content_rect();
        Area::new("remoteops_ai_settings_scrim".into())
            .order(Order::Middle)
            .fixed_pos(content_rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(content_rect.size());
                let response = ui.allocate_rect(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, content_rect.size()),
                    Sense::click(),
                );
                ui.painter().rect_filled(
                    response.rect,
                    0.0,
                    Color32::from_black_alpha(if ctx.theme() == egui::Theme::Dark {
                        150
                    } else {
                        110
                    }),
                );
                response.on_hover_cursor(CursorIcon::NotAllowed);
            });

        let dialog_height = (content_rect.height() - 36.0).clamp(460.0, 580.0);
        egui::Window::new("remoteops_ai_settings_dialog")
            .order(Order::Foreground)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(590.0, dialog_height))
            .frame(
                Frame::new()
                    .fill(palette.surface_raised)
                    .stroke(Stroke::new(1.0, palette.line_strong))
                    .corner_radius(10.0)
                    .inner_margin(Margin::same(22))
                    .shadow(palette.shadow_large),
            )
            .show(ctx, |ui| {
                ui.set_width(546.0);
                ui.horizontal(|ui| {
                    ui.label(icon(icons::SPARKLE, 22.0, palette.blue));
                    ui.label(
                        RichText::new(self.translator.text("controller.ai_settings.title"))
                            .size(20.0)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if styled_button(
                            ui,
                            icon(icons::X, 18.0, palette.text_muted),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::splat(44.0),
                        )
                        .clicked()
                            && !self.ai_test_in_progress
                        {
                            self.ai_settings_open = false;
                        }
                    });
                });
                ui.label(
                    RichText::new(self.translator.text("controller.ai_settings.description"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ui.add_space(14.0);

                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(dialog_height - 142.0)
                    .show(ui, |ui| {
                        ui.set_width(546.0);
                        ui.label(
                            RichText::new(self.translator.text("controller.ai_settings.protocol"))
                                .size(13.0)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            for protocol in [
                                AiProtocol::Auto,
                                AiProtocol::Responses,
                                AiProtocol::ChatCompletions,
                            ] {
                                let selected = self.ai_settings_form.protocol == protocol;
                                if styled_button(
                                    ui,
                                    ai_protocol_label(protocol, &self.translator),
                                    if selected {
                                        ButtonKind::Secondary
                                    } else {
                                        ButtonKind::Ghost
                                    },
                                    palette,
                                    Vec2::new(170.0, 40.0),
                                )
                                .clicked()
                                {
                                    self.ai_settings_form.protocol = protocol;
                                    self.ai_settings_message = None;
                                }
                            }
                        });
                        ui.add_space(14.0);

                        ui.label(
                            RichText::new(self.translator.text("controller.ai_settings.base_url"))
                                .size(13.0)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        if ui
                            .add(
                                TextEdit::singleline(&mut self.ai_settings_form.base_url)
                                    .hint_text(
                                        self.translator
                                            .text("controller.ai_settings.base_url_hint"),
                                    )
                                    .desired_width(f32::INFINITY),
                            )
                            .changed()
                        {
                            self.ai_settings_message = None;
                        }
                        ui.add_space(12.0);

                        ui.label(
                            RichText::new(self.translator.text("controller.ai_settings.model"))
                                .size(13.0)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        if ui
                            .add(
                                TextEdit::singleline(&mut self.ai_settings_form.model)
                                    .hint_text(
                                        self.translator.text("controller.ai_settings.model_hint"),
                                    )
                                    .desired_width(f32::INFINITY),
                            )
                            .changed()
                        {
                            self.ai_settings_message = None;
                        }
                        ui.add_space(12.0);

                        ui.label(RichText::new("Bearer Token / API Key").size(13.0).strong());
                        ui.add_space(6.0);
                        if ui
                            .add(
                                TextEdit::singleline(&mut self.ai_token_input)
                                    .password(true)
                                    .hint_text(if self.ai_token_available {
                                        self.translator
                                            .text("controller.ai_settings.token_saved_hint")
                                    } else {
                                        self.translator
                                            .text("controller.ai_settings.token_input_hint")
                                    })
                                    .desired_width(f32::INFINITY),
                            )
                            .changed()
                        {
                            self.pending_import_token = None;
                            self.ai_settings_message = None;
                        }
                        ui.label(
                            RichText::new(if self.pending_import_token.is_some() {
                                self.translator
                                    .text("controller.ai_settings.token_imported")
                            } else if self.ai_token_available {
                                self.translator
                                    .text("controller.ai_settings.token_available")
                            } else {
                                self.translator
                                    .text("controller.ai_settings.token_security")
                            })
                            .size(11.0)
                            .color(palette.text_muted),
                        );
                        ui.add_space(14.0);

                        ui.horizontal(|ui| {
                            if styled_button(
                                ui,
                                self.translator.text("controller.ai_settings.import"),
                                ButtonKind::Secondary,
                                palette,
                                Vec2::new(190.0, 42.0),
                            )
                            .clicked()
                            {
                                self.import_codex_ai_settings();
                            }
                            if ui
                                .add_enabled_ui(!self.ai_test_in_progress, |ui| {
                                    styled_button(
                                        ui,
                                        if self.ai_test_in_progress {
                                            self.translator
                                                .text("controller.ai_settings.testing_short")
                                        } else {
                                            self.translator.text("controller.ai_settings.test")
                                        },
                                        ButtonKind::Ghost,
                                        palette,
                                        Vec2::new(138.0, 42.0),
                                    )
                                })
                                .inner
                                .clicked()
                            {
                                self.test_ai_settings();
                            }
                            if styled_button(
                                ui,
                                self.translator.text("controller.ai_settings.delete"),
                                ButtonKind::Ghost,
                                palette,
                                Vec2::new(120.0, 42.0),
                            )
                            .clicked()
                            {
                                self.clear_ai_settings();
                            }
                        });

                        if let Some((message, success)) = &self.ai_settings_message {
                            ui.add_space(12.0);
                            Frame::new()
                                .fill(if *success {
                                    palette.blue_soft
                                } else {
                                    palette.red_soft
                                })
                                .stroke(Stroke::new(
                                    1.0,
                                    if *success {
                                        palette.blue_border
                                    } else {
                                        palette.red
                                    },
                                ))
                                .corner_radius(10.0)
                                .inner_margin(Margin::symmetric(12, 10))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(message).size(12.0).color(if *success {
                                            palette.text
                                        } else {
                                            palette.red
                                        }),
                                    );
                                });
                        }
                    });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if styled_button(
                        ui,
                        self.translator.text("controller.action.cancel"),
                        ButtonKind::Ghost,
                        palette,
                        Vec2::new(110.0, 44.0),
                    )
                    .clicked()
                        && !self.ai_test_in_progress
                    {
                        self.ai_settings_open = false;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if styled_button(
                            ui,
                            self.translator.text("controller.ai_settings.save"),
                            ButtonKind::Primary,
                            palette,
                            Vec2::new(150.0, 44.0),
                        )
                        .clicked()
                            && !self.ai_test_in_progress
                        {
                            self.save_ai_settings();
                        }
                    });
                });
            });
    }

    /// 校验并提交一项手动配对请求。
    fn submit_pairing(&mut self) {
        if self.demo || !self.connected || self.pairing_in_progress {
            return;
        }
        let Ok((pairing_code, alias)) = pairing_request(&self.pairing_code, &self.pairing_alias)
        else {
            self.pairing_error = Some(self.translator.text("controller.pairing.invalid_code"));
            return;
        };
        self.pairing_error = None;
        self.pairing_in_progress = true;
        self.send(BackendCommand::Pair {
            pairing_code,
            alias,
        });
    }

    /// 渲染添加连接弹窗。
    fn render_pairing_dialog(&mut self, ctx: &egui::Context, palette: Palette) {
        if !self.pairing_dialog_open {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) && !self.pairing_in_progress {
            self.pairing_dialog_open = false;
            self.pairing_error = None;
            return;
        }

        let mut submit = false;
        let mut cancel = false;
        let content_rect = ctx.content_rect();
        Area::new("remoteops_pairing_scrim".into())
            .order(Order::Middle)
            .fixed_pos(content_rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(content_rect.size());
                let response = ui.allocate_rect(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, content_rect.size()),
                    Sense::click(),
                );
                ui.painter().rect_filled(
                    response.rect,
                    0.0,
                    Color32::from_black_alpha(if ctx.theme() == egui::Theme::Dark {
                        150
                    } else {
                        110
                    }),
                );
                response.on_hover_cursor(CursorIcon::NotAllowed);
            });
        egui::Window::new("remoteops_pairing_dialog")
            .order(Order::Foreground)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(420.0, 380.0))
            .frame(
                Frame::new()
                    .fill(palette.surface_raised)
                    .stroke(Stroke::new(1.0, palette.line_strong))
                    .corner_radius(8.0)
                    .inner_margin(Margin::same(24))
                    .shadow(palette.shadow_large),
            )
            .show(ctx, |ui| {
                (submit, cancel) = self.render_pairing_dialog_content(ui, palette);
            });

        if cancel {
            self.pairing_dialog_open = false;
            self.pairing_error = None;
        } else if submit {
            self.submit_pairing();
        }
    }

    /// 渲染配对弹窗内容并返回提交、取消动作。
    fn render_pairing_dialog_content(&mut self, ui: &mut Ui, palette: Palette) -> (bool, bool) {
        let mut cancel = false;
        ui.set_width(372.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(self.translator.text("controller.pairing.title"))
                    .size(19.0)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let close = ui
                    .add_enabled_ui(!self.pairing_in_progress, |ui| {
                        styled_button(
                            ui,
                            icon(icons::X, 18.0, palette.text_muted),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::splat(44.0),
                        )
                    })
                    .inner;
                if close
                    .on_hover_text(self.translator.text("controller.pairing.cancel"))
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        ui.add_space(16.0);

        ui.label(
            RichText::new(self.translator.text("controller.pairing.code"))
                .size(13.0)
                .strong(),
        );
        ui.add_space(6.0);
        let code_input = ui
            .add_enabled_ui(!self.pairing_in_progress, |ui| {
                ui.add_sized(
                    [ui.available_width(), 44.0],
                    TextEdit::singleline(&mut self.pairing_code)
                        .hint_text(self.translator.text("controller.pairing.code_hint"))
                        .char_limit(11)
                        .margin(Margin::symmetric(8, 0))
                        .vertical_align(Align::Center)
                        .desired_width(f32::INFINITY),
                )
            })
            .inner;
        if self.pairing_focus_requested {
            code_input.request_focus();
            self.pairing_focus_requested = false;
        }
        if code_input.changed() {
            self.pairing_error = None;
        }
        if let Some(error) = &self.pairing_error {
            ui.add_space(4.0);
            ui.label(RichText::new(error).size(12.0).color(palette.red));
        } else {
            ui.add_space(20.0);
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new(self.translator.text("controller.pairing.alias"))
                .size(13.0)
                .strong(),
        );
        ui.add_space(6.0);
        ui.add_enabled_ui(!self.pairing_in_progress, |ui| {
            ui.add_sized(
                [ui.available_width(), 44.0],
                TextEdit::singleline(&mut self.pairing_alias)
                    .hint_text(self.translator.text("controller.pairing.alias_hint"))
                    .char_limit(80)
                    .margin(Margin::symmetric(8, 0))
                    .vertical_align(Align::Center)
                    .desired_width(f32::INFINITY),
            )
        });
        ui.add_space(10.0);
        if self.connected {
            ui.label(
                RichText::new(self.translator.text("controller.pairing.connected"))
                    .size(12.0)
                    .color(palette.green),
            );
        } else {
            ui.label(
                RichText::new(self.translator.text("controller.pairing.connecting"))
                    .size(12.0)
                    .color(palette.amber),
            );
        }
        let (submit, footer_cancel) = self.render_pairing_dialog_actions(ui, palette);
        (submit, cancel || footer_cancel)
    }

    /// 渲染配对弹窗底部操作并返回提交、取消动作。
    fn render_pairing_dialog_actions(&mut self, ui: &mut Ui, palette: Palette) -> (bool, bool) {
        let mut submit = false;
        let mut cancel = false;
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            if icon_button(
                ui,
                icons::X,
                &self.translator.text("controller.pairing.cancel"),
                ButtonKind::Ghost,
                palette,
                Vec2::new(100.0, 44.0),
            )
            .clicked()
                && !self.pairing_in_progress
            {
                cancel = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.pairing_in_progress {
                    self.translator.text("controller.pairing.connecting_button")
                } else {
                    self.translator.text("controller.pairing.connect_button")
                };
                if ui
                    .add_enabled_ui(!self.pairing_in_progress && self.connected, |ui| {
                        icon_button(
                            ui,
                            icons::PLUGS_CONNECTED,
                            &label,
                            ButtonKind::Primary,
                            palette,
                            Vec2::new(124.0, 44.0),
                        )
                    })
                    .inner
                    .clicked()
                {
                    submit = true;
                }
            });
        });

        if self.connected && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        (submit, cancel)
    }

    /// 保存主题设置。
    fn save_settings(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            "remoteops_gui_settings",
            &GuiSettings {
                theme_mode: self.theme_mode,
                send_shortcut: self.send_shortcut,
                language: Some(self.translator.language()),
            },
        );
    }

    /// 渲染独立串口原生窗口。
    fn render_serial_workbench(&mut self, ctx: &egui::Context, palette: Palette) {
        if !self.serial_workbench.open {
            return;
        }
        let viewport_id = egui::ViewportId::from_hash_of("remoteops_serial_workbench");
        let mut workbench = std::mem::take(&mut self.serial_workbench);
        ctx.show_viewport_immediate(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title(self.translator.text("controller.serial.title"))
                .with_inner_size(Vec2::new(1180.0, 760.0))
                .with_min_inner_size(Vec2::new(900.0, 600.0))
                .with_drag_and_drop(false),
            |ui, _class| {
                if let Some(command) = workbench.render(ui, palette, &self.translator) {
                    if let BackendCommand::DecideApproval { approval, .. } = &command {
                        self.approvals
                            .retain(|item| item.approval_id != approval.approval_id);
                    }
                    let _ = self.backend.commands.send(command);
                }
            },
        );
        self.serial_workbench = workbench;
    }
}

impl eframe::App for RemoteOpsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        install_design_style(&ctx);
        let palette = current_palette(&ctx);
        self.process_backend_events(&ctx);
        self.render_sidebar(ui, palette);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(palette.background).inner_margin(Margin {
                left: 20,
                right: 20,
                top: 18,
                bottom: 28,
            }))
            .show(ui, |ui| {
                self.render_header(ui, palette);
                self.render_mode_note(ui, palette);
                self.render_timeline(ui, palette);
            });
        self.render_pairing_dialog(&ctx, palette);
        self.render_ai_settings_dialog(&ctx, palette);
        self.render_serial_workbench(&ctx, palette);

        if let Some((message, remaining)) = &mut self.toast {
            *remaining -= ctx.input(|input| input.stable_dt);
            Area::new("remoteops_toast".into())
                .order(Order::Tooltip)
                .anchor(egui::Align2::RIGHT_TOP, [-24.0, 124.0])
                .show(&ctx, |ui| {
                    ui.set_max_width(320.0);
                    card(
                        palette,
                        palette.surface_raised,
                        palette.line_strong,
                        12,
                        Margin::symmetric(14, 11),
                    )
                    .show(ui, |ui| {
                        ui.label(RichText::new(message.as_str()).size(13.0).strong());
                    });
                });
            if *remaining <= 0.0 {
                self.toast = None;
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.save_settings(storage);
    }
}

/// 校验并规范化配对弹窗的输入。
fn pairing_request(code: &str, alias: &str) -> Result<(String, Option<String>), &'static str> {
    let code = PairingCode::parse(code.trim()).map_err(|_| "请输入正确的九位控制码")?;
    let alias = alias.trim();
    Ok((
        code.to_string(),
        (!alias.is_empty()).then(|| alias.to_owned()),
    ))
}

/// 渲染一条 Human、AI 或 System 协作消息。
#[allow(clippy::too_many_lines)]
fn render_event(ui: &mut Ui, event: &RemoteEvent, palette: Palette, translator: &Translator) {
    let (source, source_icon, source_color) = match event.source {
        EventSource::Human => (
            translator.text("controller.source.human"),
            icons::USER_CIRCLE,
            palette.blue,
        ),
        EventSource::Ai => (
            translator.text("controller.source.ai"),
            icons::SPARKLE,
            palette.blue,
        ),
        EventSource::System => (
            translator.text("controller.source.system"),
            icons::INFO,
            palette.text_muted,
        ),
    };
    ui.horizontal_top(|ui| {
        ui.add_space(2.0);
        ui.label(icon(source_icon, 23.0, source_color));
        ui.add_space(3.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(source).size(14.0).strong());
                ui.label(
                    RichText::new(
                        event
                            .occurred_at
                            .with_timezone(&Local)
                            .format("%H:%M")
                            .to_string(),
                    )
                    .size(12.0)
                    .color(palette.text_faint),
                );
            });
            ui.add_space(4.0);
            match &event.payload {
                EventPayload::OperationRequested { operation } => {
                    let summary = if event.source == EventSource::Ai
                        && matches!(
                            operation,
                            RemoteOperation::RunCommand {
                                readonly: false,
                                ..
                            }
                        ) {
                        translator.text("controller.timeline.demo_cleanup_prompt")
                    } else {
                        operation_summary(operation, translator)
                    };
                    ui.label(RichText::new(summary).size(15.0).color(palette.text));
                }
                EventPayload::ApprovalRequired { reason, .. } => {
                    ui.label(
                        RichText::new(if event.source == EventSource::Ai {
                            translator.text("controller.timeline.ai_approval_required")
                        } else {
                            translator.text("controller.timeline.human_approval_required")
                        })
                        .size(15.0),
                    );
                    if !reason.is_empty() {
                        ui.label(RichText::new(reason).size(12.0).color(palette.text_muted));
                    }
                }
                EventPayload::OperationStarted => {
                    ui.label(
                        RichText::new(translator.text("controller.timeline.operation_started"))
                            .size(15.0),
                    );
                }
                EventPayload::OutputChunk { stderr, text } => {
                    render_terminal_result(ui, text, *stderr, palette, translator);
                }
                EventPayload::OperationCompleted { exit_code, summary } => {
                    let fill = if exit_code.unwrap_or_default() == 0 {
                        palette.green_soft
                    } else {
                        palette.red_soft
                    };
                    let color = if exit_code.unwrap_or_default() == 0 {
                        palette.green
                    } else {
                        palette.red
                    };
                    Frame::new()
                        .fill(fill)
                        .corner_radius(10.0)
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("{}  {summary}", icons::CHECK_CIRCLE))
                                    .size(13.0)
                                    .strong()
                                    .color(color),
                            );
                        });
                }
                EventPayload::OperationFailed { code, message } => {
                    Frame::new()
                        .fill(palette.red_soft)
                        .corner_radius(10.0)
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{}  {code}：{message}",
                                        icons::X_CIRCLE
                                    ))
                                    .size(13.0)
                                    .strong()
                                    .color(palette.red),
                                )
                                .wrap(),
                            );
                        });
                }
                EventPayload::OperationCancelled => {
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            icons::STOP_CIRCLE,
                            translator.text("controller.timeline.cancelled")
                        ))
                        .size(13.0)
                        .strong()
                        .color(palette.red),
                    );
                }
                EventPayload::SessionOpened => {
                    ui.label(
                        RichText::new(translator.text("controller.timeline.session_opened"))
                            .size(14.0),
                    );
                }
                EventPayload::SessionDisconnected => {
                    ui.label(
                        RichText::new(translator.text("controller.timeline.session_disconnected"))
                            .size(14.0)
                            .color(palette.text_muted),
                    );
                }
                EventPayload::SessionResumed => {
                    ui.label(
                        RichText::new(translator.text("controller.timeline.session_resumed"))
                            .size(14.0),
                    );
                }
                EventPayload::SessionClosed => {
                    ui.label(
                        RichText::new(translator.text("controller.timeline.session_closed"))
                            .size(14.0)
                            .color(palette.text_muted),
                    );
                }
            }
        });
    });
}

/// 渲染按请求聚合后的终端任务。
#[allow(clippy::too_many_lines)]
fn render_request_timeline_item(
    ui: &mut Ui,
    request: &RequestTimelineItem,
    palette: Palette,
    translator: &Translator,
) {
    let (source, source_icon, source_color) = match request.source {
        EventSource::Human => (
            translator.text("controller.source.human"),
            icons::USER_CIRCLE,
            palette.blue,
        ),
        EventSource::Ai => (
            translator.text("controller.source.ai"),
            icons::SPARKLE,
            palette.blue,
        ),
        EventSource::System => (
            translator.text("controller.source.system"),
            icons::INFO,
            palette.text_muted,
        ),
    };
    ui.horizontal_top(|ui| {
        ui.add_space(2.0);
        ui.label(icon(source_icon, 23.0, source_color));
        ui.add_space(3.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(source).size(14.0).strong());
                ui.label(
                    RichText::new(
                        request
                            .occurred_at
                            .with_timezone(&Local)
                            .format("%H:%M")
                            .to_string(),
                    )
                    .size(12.0)
                    .color(palette.text_faint),
                );
            });
            if let Some(operation) = request.operation.as_ref() {
                ui.add_space(4.0);
                ui.label(RichText::new(operation_summary(operation, translator)).size(15.0));
            }
            if request.started && request.output.is_empty() && request.terminal.is_none() {
                ui.label(
                    RichText::new(translator.text("controller.timeline.executing"))
                        .size(13.0)
                        .color(palette.text_muted),
                );
            }
            for segment in &request.output {
                render_terminal_result(ui, &segment.text, segment.stderr, palette, translator);
                ui.add_space(4.0);
            }
            if let Some(terminal) = &request.terminal {
                match terminal {
                    RequestTerminal::Completed { exit_code, .. } => {
                        let success = exit_code.unwrap_or_default() == 0;
                        ui.label(
                            RichText::new(format!(
                                "{}  {} · {} {}",
                                if success {
                                    icons::CHECK_CIRCLE
                                } else {
                                    icons::WARNING
                                },
                                translator.text("controller.timeline.completed"),
                                translator.text("controller.timeline.exit_code"),
                                exit_code.map_or_else(
                                    || translator.text("controller.unknown"),
                                    |code| code.to_string(),
                                )
                            ))
                            .size(13.0)
                            .strong()
                            .color(if success {
                                palette.green
                            } else {
                                palette.amber
                            }),
                        );
                    }
                    RequestTerminal::Failed { code, message } => {
                        ui.label(
                            RichText::new(format!("{}  {code}：{message}", icons::X_CIRCLE))
                                .size(13.0)
                                .strong()
                                .color(palette.red),
                        );
                    }
                    RequestTerminal::Cancelled => {
                        ui.label(
                            RichText::new(format!(
                                "{}  {}",
                                icons::STOP_CIRCLE,
                                translator.text("controller.timeline.cancelled")
                            ))
                            .size(13.0)
                            .strong()
                            .color(palette.red),
                        );
                    }
                }
            }
        });
    });
}

/// 按聊天顺序渲染用户问题、AI 工具状态和最终回答。
fn render_ai_interaction(
    ui: &mut Ui,
    interaction: &AiInteraction,
    palette: Palette,
    translator: &Translator,
) -> Vec<RequestId> {
    render_ai_user_bubble(ui, interaction, palette, translator);
    ui.add_space(8.0);
    render_ai_assistant_bubble(ui, interaction, palette, translator)
}

/// 渲染位于右侧的用户问题气泡。
fn render_ai_user_bubble(
    ui: &mut Ui,
    interaction: &AiInteraction,
    palette: Palette,
    translator: &Translator,
) -> egui::Rect {
    let max_width = (ui.available_width() * 0.52).clamp(260.0, 600.0);
    let user_bubble_width = chat_text_width(ui, &interaction.prompt, 13.5, 110.0, max_width, 28.0);
    // 让消息内容参与正常布局并自然撑高，避免长文本或 DPI 缩放超过手工估算高度。
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        render_chat_avatar(
            ui,
            &translator.text("controller.chat.you"),
            palette.blue,
            Color32::WHITE,
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(user_bubble_width, 0.0),
            Layout::top_down(Align::Max),
            |ui| {
                ui.set_width(user_bubble_width);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(interaction.occurred_at.format("%H:%M").to_string())
                            .size(10.5)
                            .color(palette.text_faint),
                    );
                    ui.label(
                        RichText::new(translator.text("controller.chat.you"))
                            .size(11.5)
                            .strong()
                            .color(palette.text_muted),
                    );
                });
                ui.add_space(4.0);
                Frame::new()
                    .fill(palette.blue)
                    .corner_radius(12.0)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width((user_bubble_width - 28.0).max(1.0));
                        ui.add(
                            egui::Label::new(
                                RichText::new(&interaction.prompt)
                                    .size(13.5)
                                    .color(Color32::WHITE),
                            )
                            .wrap(),
                        );
                    })
                    .response
                    .rect
            },
        )
        .inner
    })
    .inner
}

/// 渲染位于左侧的 AI 回答气泡及其工具执行状态。
fn render_ai_assistant_bubble(
    ui: &mut Ui,
    interaction: &AiInteraction,
    palette: Palette,
    translator: &Translator,
) -> Vec<RequestId> {
    let mut toggled_tools = Vec::new();
    let max_width = (ui.available_width() * 0.64).clamp(320.0, 720.0);
    let ai_bubble_width = ai_interaction_width(ui, interaction, max_width, translator);
    let failed = matches!(interaction.status, AiInteractionStatus::Failed(_));
    // 使用正常的横向/纵向布局，让工具详情和回答的真实高度推进时间线游标。
    ui.horizontal_top(|ui| {
        render_chat_avatar(
            ui,
            icons::SPARKLE,
            if failed { palette.red } else { palette.blue },
            Color32::WHITE,
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ai_bubble_width, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(ai_bubble_width);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("RemoteOps AI")
                            .size(11.5)
                            .strong()
                            .color(palette.text_muted),
                    );
                    ui.label(
                        RichText::new(interaction.occurred_at.format("%H:%M").to_string())
                            .size(10.5)
                            .color(palette.text_faint),
                    );
                });
                ui.add_space(4.0);
                Frame::new()
                    .fill(if failed {
                        palette.red_soft
                    } else {
                        palette.surface_raised
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if failed { palette.red } else { palette.line },
                    ))
                    .corner_radius(12.0)
                    .inner_margin(Margin::symmetric(14, 11))
                    .show(ui, |ui| {
                        ui.set_width((ai_bubble_width - 28.0).max(1.0));
                        for tool in &interaction.tools {
                            if render_ai_tool_run(ui, tool, palette, translator) {
                                toggled_tools.push(tool.request_id);
                            }
                        }
                        ui.add_space(if interaction.tools.is_empty() {
                            2.0
                        } else {
                            9.0
                        });
                        render_ai_status(ui, &interaction.status, palette, translator);
                    });
            },
        );
    });
    toggled_tools
}

/// 根据消息内容计算不会撑满整行的气泡宽度。
fn chat_text_width(
    ui: &Ui,
    text: &str,
    font_size: f32,
    min_width: f32,
    max_width: f32,
    horizontal_padding: f32,
) -> f32 {
    let wrap_width = (max_width - horizontal_padding).max(1.0);
    let galley = ui.painter().layout(
        text.to_owned(),
        egui::FontId::proportional(font_size),
        Color32::WHITE,
        wrap_width,
    );
    (galley.size().x + horizontal_padding).clamp(min_width, max_width)
}

/// 结合回答和工具命令计算 AI 消息列宽。
fn ai_interaction_width(
    ui: &Ui,
    interaction: &AiInteraction,
    max_width: f32,
    translator: &Translator,
) -> f32 {
    let status_text = match &interaction.status {
        AiInteractionStatus::Waiting => translator.text("controller.ai_status.waiting"),
        AiInteractionStatus::RunningTool => translator.text("controller.ai_status.running_tool"),
        AiInteractionStatus::Completed(answer) => answer.clone(),
        AiInteractionStatus::Failed(error) => error.clone(),
    };
    let mut width = chat_text_width(ui, &status_text, 13.5, 320.0, max_width, 28.0);
    for tool in &interaction.tools {
        let tool_text = format!(
            "{}  {}  {} 000  {}",
            translator.text("controller.ai_tool.completed"),
            tool.command,
            translator.text("controller.timeline.exit_code"),
            translator.text("controller.ai_tool.details"),
        );
        width = width.max(chat_text_width(
            ui, &tool_text, 12.0, 320.0, max_width, 52.0,
        ));
    }
    width
}

/// 渲染聊天消息旁的固定方形头像。
fn render_chat_avatar(ui: &mut Ui, label: &str, fill: Color32, foreground: Color32) -> egui::Rect {
    // `centered_and_justified` 会继承滚动区的剩余空间，宽屏下可能把头像拉成整屏。
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
    ui.painter().rect_filled(rect, 9.0, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        foreground,
    );
    rect
}

/// 渲染一项 AI 远程工具执行记录，返回用户是否点击了详情开关。
fn render_ai_tool_run(
    ui: &mut Ui,
    tool: &AiToolRun,
    palette: Palette,
    translator: &Translator,
) -> bool {
    ui.add_space(8.0);
    let finished = tool.summary.is_some();
    let row = Frame::new()
        .fill(palette.surface_muted)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(10.0)
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            render_ai_tool_header(ui, tool, finished, palette, translator);
        });
    let toggled = finished
        && row
            .response
            .interact(Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .clicked();
    if tool.expanded
        && let Some(summary) = &tool.summary
    {
        render_ai_tool_details(ui, summary, palette);
    }
    toggled
}

/// 渲染 AI 工具执行记录的标题行。
fn render_ai_tool_header(
    ui: &mut Ui,
    tool: &AiToolRun,
    finished: bool,
    palette: Palette,
    translator: &Translator,
) {
    ui.horizontal(|ui| {
        ui.label(icon(
            if finished {
                icons::CHECK_CIRCLE
            } else {
                icons::MAGNIFYING_GLASS
            },
            15.0,
            if finished {
                palette.green
            } else {
                palette.blue
            },
        ));
        ui.label(
            RichText::new(if finished {
                translator.text("controller.ai_tool.completed")
            } else {
                translator.text("controller.ai_tool.running")
            })
            .size(12.0)
            .strong()
            .color(palette.text_muted),
        );
        ui.label(
            RichText::new(&tool.command)
                .monospace()
                .size(12.0)
                .color(palette.text),
        );
        if let Some(exit_code) = tool.exit_code {
            ui.label(
                RichText::new(format!(
                    "{} {exit_code}",
                    translator.text("controller.timeline.exit_code")
                ))
                .size(11.0)
                .color(if exit_code == 0 {
                    palette.green
                } else {
                    palette.amber
                }),
            );
        }
        if finished {
            render_ai_tool_toggle(ui, tool.expanded, palette, translator);
        }
    });
}

/// 渲染 AI 工具详情开关提示。
fn render_ai_tool_toggle(ui: &mut Ui, expanded: bool, palette: Palette, translator: &Translator) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(icon(
            if expanded {
                icons::CARET_UP
            } else {
                icons::CARET_DOWN
            },
            14.0,
            palette.text_faint,
        ));
        ui.label(
            RichText::new(if expanded {
                translator.text("controller.ai_tool.collapse")
            } else {
                translator.text("controller.ai_tool.details")
            })
            .size(11.0)
            .color(palette.text_muted),
        );
    });
}

/// 渲染 AI 工具的原始远程输出。
fn render_ai_tool_details(ui: &mut Ui, summary: &str, palette: Palette) {
    ui.add_space(6.0);
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(9.0)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(summary.trim_end())
                        .monospace()
                        .size(11.5)
                        .color(palette.text_muted),
                )
                .wrap(),
            );
        });
}

/// 渲染 AI 当前状态或最终回答。
fn render_ai_status(
    ui: &mut Ui,
    status: &AiInteractionStatus,
    palette: Palette,
    translator: &Translator,
) {
    match status {
        AiInteractionStatus::Waiting => {
            ui.label(
                RichText::new(translator.text("controller.ai_status.waiting"))
                    .size(13.0)
                    .color(palette.text_muted),
            );
        }
        AiInteractionStatus::RunningTool => {
            ui.label(
                RichText::new(translator.text("controller.ai_status.running_tool"))
                    .size(13.0)
                    .color(palette.text_muted),
            );
        }
        AiInteractionStatus::Completed(answer) => {
            ui.add(
                egui::Label::new(
                    RichText::new(normalize_ai_answer(answer))
                        .size(13.5)
                        .color(palette.text),
                )
                .wrap(),
            );
        }
        AiInteractionStatus::Failed(error) => {
            ui.add(egui::Label::new(RichText::new(error).size(13.0).color(palette.red)).wrap());
        }
    }
}

/// 清理当前纯文本气泡无法表达的常见 Markdown 强调符号。
fn normalize_ai_answer(answer: &str) -> String {
    answer.replace("**", "")
}

/// 渲染终端输出卡片。
fn render_terminal_result(
    ui: &mut Ui,
    text: &str,
    stderr: bool,
    palette: Palette,
    translator: &Translator,
) {
    Frame::new()
        .fill(palette.surface_muted)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(14.0)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_min_width((ui.available_width() - 4.0).max(320.0));
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("›_")
                        .monospace()
                        .size(13.0)
                        .strong()
                        .color(if stderr { palette.red } else { palette.blue }),
                );
                ui.label(
                    RichText::new(if stderr {
                        translator.text("controller.terminal.stderr")
                    } else {
                        translator.text("controller.terminal.stdout")
                    })
                    .monospace()
                    .size(13.0)
                    .color(palette.text_muted),
                );
            });
            ui.add_space(10.0);
            ui.add(
                egui::Label::new(RichText::new(text.trim_end()).monospace().size(12.5).color(
                    if stderr {
                        palette.red
                    } else {
                        palette.text_muted
                    },
                ))
                .wrap(),
            );
        });
}

/// 渲染一项等待人工决定的审批。
fn render_approval(
    app: &mut RemoteOpsApp,
    ui: &mut Ui,
    approval: &PendingApproval,
    palette: Palette,
) {
    let mut decision = None;
    let compact = ui.ctx().content_rect().height() < 900.0;
    Frame::new()
        .fill(palette.amber_soft)
        .stroke(Stroke::new(1.0, palette.amber))
        .corner_radius(15.0)
        .inner_margin(Margin::symmetric(20, if compact { 11 } else { 16 }))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(icon(icons::SHIELD_CHECK, 20.0, palette.amber));
                        ui.label(
                            RichText::new(app.translator.text("controller.approval.required"))
                                .size(15.0)
                                .strong(),
                        );
                    });
                    if compact {
                        ui.label(
                            RichText::new(&approval.reason)
                                .size(12.0)
                                .color(palette.text),
                        );
                    } else {
                        ui.add_space(7.0);
                        ui.label(
                            RichText::new(if approval.source == EventSource::Ai {
                                app.translator.text("controller.approval.ai_request")
                            } else {
                                app.translator.text("controller.approval.human_request")
                            })
                            .size(13.0)
                            .color(palette.text_muted),
                        );
                        ui.label(
                            RichText::new(&approval.reason)
                                .size(13.0)
                                .color(palette.text),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                operation_summary(&approval.operation, &app.translator),
                                app.translator.text("controller.approval.session_bound")
                            ))
                            .size(11.0)
                            .color(palette.amber),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::BOTTOM), |ui| {
                    if icon_button(
                        ui,
                        icons::CHECK,
                        &app.translator.text("controller.approval.approve"),
                        ButtonKind::Primary,
                        palette,
                        Vec2::new(116.0, if compact { 36.0 } else { 44.0 }),
                    )
                    .clicked()
                    {
                        decision = Some(true);
                    }
                    if icon_button(
                        ui,
                        icons::X,
                        &app.translator.text("controller.approval.reject"),
                        ButtonKind::Ghost,
                        palette,
                        Vec2::new(116.0, if compact { 36.0 } else { 44.0 }),
                    )
                    .clicked()
                    {
                        decision = Some(false);
                    }
                });
            });
        });

    if let Some(approved) = decision {
        app.send(BackendCommand::DecideApproval {
            approval: Box::new(approval.clone()),
            approved,
        });
        app.approvals
            .retain(|item| item.approval_id != approval.approval_id);
        app.serial_workbench
            .resolve_approval(approval.approval_id, approved);
    }
}

/// 返回适合人类阅读的操作摘要。
fn operation_summary(operation: &RemoteOperation, translator: &Translator) -> String {
    match operation {
        RemoteOperation::RunCommand {
            command, readonly, ..
        } => {
            if *readonly {
                command.clone()
            } else {
                format!(
                    "{}：{command}",
                    translator.text("controller.operation.change_command")
                )
            }
        }
        _ => format!("{operation:?}"),
    }
}

/// 返回缩短后的会话标识。
fn short_session_id(session_id: SessionId) -> String {
    let value = session_id.to_string();
    format!("{}…", &value[..18])
}

/// 返回连接状态的中文名称。
fn connection_state_label(state: ConnectionState, translator: &Translator) -> String {
    translator.text(match state {
        ConnectionState::Online => "controller.connection.online",
        ConnectionState::Reconnecting => "controller.connection.reconnecting",
        ConnectionState::Offline => "controller.connection.offline",
        ConnectionState::Closed => "controller.connection.closed",
    })
}

fn theme_mode_label(mode: ThemeMode, translator: &Translator) -> String {
    translator.text(match mode {
        ThemeMode::System => "controller.theme.system",
        ThemeMode::Light => "controller.theme.light",
        ThemeMode::Dark => "controller.theme.dark",
    })
}

fn ai_protocol_label(protocol: AiProtocol, translator: &Translator) -> String {
    translator.text(match protocol {
        AiProtocol::Auto => "controller.ai.protocol.auto",
        AiProtocol::Responses => "controller.ai.protocol.responses",
        AiProtocol::ChatCompletions => "controller.ai.protocol.chat_completions",
    })
}

fn main() -> eframe::Result {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("remoteops-controller-gui")
            .with_inner_size(Vec2::new(1100.0, 680.0))
            .with_min_inner_size(Vec2::new(920.0, 560.0))
            .with_drag_and_drop(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let mut translator = Translator::detect();
    if let Ok(path) = std::env::var("REMOTEOPS_LANG_FILE") {
        let _ = translator.overlay_file(path);
    }
    let title = translator.text("app.controller_title");
    eframe::run_native(
        &title,
        native_options,
        Box::new(move |cc| Ok(Box::new(RemoteOpsApp::new(cc, args)))),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use chrono::Local;
    use eframe::egui::{self, Color32, Context, Modifiers, Pos2, RawInput, Rect, Vec2};
    use remoteops_domain::{RequestId, SessionId};
    use remoteops_i18n::{Language, Translator};

    use super::{
        AiInteraction, AiInteractionStatus, ApprovalMode, GuiSettings, PermissionModeArg,
        SendShortcut, current_palette, normalize_ai_answer, pairing_request, render_ai_user_bubble,
        render_chat_avatar, send_shortcut_matches, timeline_conversation_height,
    };

    /// 在指定逻辑窗口尺寸中运行一帧真实 `egui` 布局。
    fn run_layout_test(size: Vec2, render: impl FnOnce(&mut egui::Ui)) {
        let context = Context::default();
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut render = Some(render);
        let _ = context.run_ui(input, |ui| {
            render.take().expect("布局测试只应渲染一帧")(ui);
        });
    }

    /// 创建只包含短文本的一轮 AI 对话。
    fn short_ai_interaction() -> AiInteraction {
        AiInteraction {
            interaction_id: RequestId::new(),
            session_id: SessionId::new(),
            occurred_at: Local::now(),
            prompt: "你".to_owned(),
            tools: Vec::new(),
            status: AiInteractionStatus::Waiting,
        }
    }

    #[test]
    fn pairing_request_normalizes_code_and_optional_alias() {
        assert_eq!(
            pairing_request(" 123456789 ", " 客户 A "),
            Ok(("123-456-789".to_owned(), Some("客户 A".to_owned())))
        );
        assert_eq!(
            pairing_request("123-456-789", "   "),
            Ok(("123-456-789".to_owned(), None))
        );
    }

    #[test]
    fn pairing_request_rejects_invalid_code() {
        assert_eq!(
            pairing_request("12345678", "客户 A"),
            Err("请输入正确的九位控制码")
        );
    }

    #[test]
    fn send_shortcut_matches_selected_mode() {
        assert!(send_shortcut_matches(SendShortcut::Enter, Modifiers::NONE));
        assert!(!send_shortcut_matches(SendShortcut::Enter, Modifiers::CTRL));
        assert!(send_shortcut_matches(
            SendShortcut::CtrlEnter,
            Modifiers::CTRL
        ));
        assert!(!send_shortcut_matches(
            SendShortcut::CtrlEnter,
            Modifiers::NONE
        ));
    }

    #[test]
    fn approval_mode_matches_controller_startup_permission() {
        assert_eq!(
            ApprovalMode::from(PermissionModeArg::ReadOnly),
            ApprovalMode::ReadOnly
        );
        assert_eq!(
            ApprovalMode::from(PermissionModeArg::ApprovalRequired),
            ApprovalMode::Confirm
        );
        assert_eq!(
            ApprovalMode::from(PermissionModeArg::FullAccess),
            ApprovalMode::FullAccess
        );
    }

    #[test]
    fn legacy_gui_settings_default_to_enter_send() {
        let settings: GuiSettings =
            serde_json::from_str(r#"{"theme_mode":"System"}"#).expect("旧版 GUI 设置应继续可读");
        assert_eq!(settings.send_shortcut, SendShortcut::Enter);
    }

    #[test]
    fn ai_answer_removes_plain_text_bold_markers() {
        assert_eq!(
            normalize_ai_answer("计算机名：**LAB-WIN-A**\nIP：**192.0.2.117**"),
            "计算机名：LAB-WIN-A\nIP：192.0.2.117"
        );
    }

    #[test]
    fn timeline_height_reserves_composer_and_card_insets() {
        assert!((timeline_conversation_height(680.0, true) - 416.0).abs() < f32::EPSILON);
        assert!((timeline_conversation_height(928.0, false) - 640.0).abs() < f32::EPSILON);
        assert!((timeline_conversation_height(360.0, true) - 120.0).abs() < f32::EPSILON);
    }

    #[test]
    fn chat_avatar_stays_square_with_large_available_space() {
        let rendered_rect = Cell::new(Rect::NOTHING);
        run_layout_test(Vec2::new(2_560.0, 1_440.0), |ui| {
            rendered_rect.set(render_chat_avatar(ui, "你", Color32::BLUE, Color32::WHITE));
        });

        assert_eq!(rendered_rect.get().size(), Vec2::splat(34.0));
    }

    #[test]
    fn short_user_bubble_stays_compact_on_wide_screen() {
        let rendered_rect = Cell::new(Rect::NOTHING);
        let interaction = short_ai_interaction();
        let translator = Translator::new(Language::ZhCn);
        run_layout_test(Vec2::new(2_560.0, 1_440.0), |ui| {
            rendered_rect.set(render_ai_user_bubble(
                ui,
                &interaction,
                current_palette(ui.ctx()),
                &translator,
            ));
        });

        let size = rendered_rect.get().size();
        assert!(size.x <= 600.0, "短消息气泡宽度异常：{}", size.x);
        assert!(size.y < 80.0, "短消息气泡高度异常：{}", size.y);
    }
}
