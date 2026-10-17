//! Read-only view model and explicit host-owned actions. No runtime mutation is exposed.
use crate::{appearance::Appearance, egui};
pub use remoteops_agent::{AgentLogLevel, AgentOperationLog, AgentOperationState};
pub use remoteops_domain::RequestId;
pub use remoteops_i18n::Language;
use remoteops_protocol::{ControlSource, ControllerControlMode};
use std::collections::BTreeMap;
#[derive(Default)]
pub struct GuiOptions {
    pub icon_png: Option<&'static [u8]>,
    pub appearance: Appearance,
    pub activity: bool,
    pub layout: GuiLayout,
    pub texts: BTreeMap<Language, BTreeMap<String, String>>,
    pub extension: Option<Box<dyn GuiExtension>>,
}
/// 主界面的布局模板，保持呈现选择与运行行为分离。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuiLayout {
    /// 通用界面及可扩展活动区域。
    #[default]
    Standard,
    /// 500×375 的紧凑界面，活动区域紧接控制码且不重复展示页头图标。
    Compact,
}

impl GuiOptions {
    /// 返回启动窗口和运行中窗口共同采用的内部尺寸。
    #[must_use]
    pub fn running_size(&self) -> egui::Vec2 {
        if self.layout == GuiLayout::Standard && (self.activity || self.extension.is_some()) {
            egui::vec2(540.0, 560.0)
        } else {
            egui::vec2(500.0, 375.0)
        }
    }
}

/// 由底座过滤连接代次后发送的只读呈现事件。
#[derive(Clone, Debug)]
pub enum GuiEvent {
    /// 断开连接或开始新连接时丢弃旧动画和执行摘要。
    Reset,
    /// 至少一个控制端已连接，允许展示本连接的活动。
    ControllerConnected,
    /// 一条已由运行层脱敏的日志。
    OperationLog(AgentOperationLog),
    /// 真实执行阶段；标准错误输出本身不会结束任务。
    OperationState {
        request_id: RequestId,
        state: AgentOperationState,
    },
}
/// 定制区域与默认组件的组合方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExtensionMode {
    /// 保留默认组件，并在其后绘制定制内容。
    #[default]
    Append,
    /// 仅绘制定制内容，权限与宿主动作仍由底座处理。
    Replace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionSlot {
    Header,
    ConnectionHint,
    Activity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPhase {
    Starting,
    Connecting,
    Waiting,
    Controlled,
    Reconnecting,
    Stopped,
    Failed,
}
#[derive(Clone, Debug)]
pub struct EffectiveControl {
    pub mode: ControllerControlMode,
    pub source: Option<ControlSource>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}
#[derive(Clone, Debug)]
pub struct GuiSnapshot {
    pub phase: ConnectionPhase,
    pub control_code: Option<String>,
    pub effective_control: Option<EffectiveControl>,
    pub default_full_control: bool,
    pub running_tasks: usize,
    pub last_result: Option<AgentOperationState>,
    pub logs: Vec<AgentOperationLog>,
    pub request_pending: bool,
    pub error: Option<String>,
    pub language: Language,
}
pub enum GuiAction {
    CopyControlCode,
    OpenLogs,
    OpenPermissions,
    SaveDefaultPreference(bool),
    RequestControl(ControllerControlMode),
    StopAssistance,
}
pub trait GuiExtension: Send {
    /// 接收已过滤的呈现事件，不接管连接、权限或任务执行。
    fn observe(&mut self, _event: &GuiEvent) {}

    /// 指定区域默认追加内容；只有明确选择时才替换底座组件。
    fn mode(&self, _slot: ExtensionSlot) -> ExtensionMode {
        ExtensionMode::Append
    }

    fn render(
        &mut self,
        slot: ExtensionSlot,
        ui: &mut egui::Ui,
        state: &GuiSnapshot,
    ) -> Vec<GuiAction>;
}

impl GuiOptions {
    /// Default product presentation; custom products use `Default` and their own text.
    #[must_use]
    pub fn remoteops() -> Self {
        let texts = [Language::ZhCn, Language::EnUs]
            .into_iter()
            .map(|language| {
                let title = remoteops_i18n::Translator::new(language).text("app.agent_title");
                (
                    language,
                    BTreeMap::from([("app.agent_title".into(), title)]),
                )
            })
            .collect();
        Self {
            texts,
            ..Self::default()
        }
    }
}
