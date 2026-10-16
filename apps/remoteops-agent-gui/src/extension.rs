//! Read-only view model and explicit host-owned actions. No runtime mutation is exposed.
use crate::{appearance::Appearance, egui};
use remoteops_agent::{AgentOperationLog, AgentOperationState};
pub use remoteops_i18n::Language;
use remoteops_protocol::{ControlSource, ControllerControlMode};
use std::collections::BTreeMap;
#[derive(Default)]
pub struct GuiOptions {
    pub icon_png: Option<&'static [u8]>,
    pub appearance: Appearance,
    pub activity: bool,
    pub texts: BTreeMap<Language, BTreeMap<String, String>>,
    pub extension: Option<Box<dyn GuiExtension>>,
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
