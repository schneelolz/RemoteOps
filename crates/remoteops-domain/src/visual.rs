//! AI 图形 Provider 共享领域模型。

use serde::{Deserialize, Serialize};

use crate::{RequestId, SessionId};

/// 交互式桌面可提供的图形能力。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualCapability {
    /// 获取窗口和控件结构。
    UiAutomation,
    /// 获取按需截图。
    Screenshot,
    /// 调用 UIA 控件模式。
    ControlInvoke,
    /// 向已验证文本控件输入文字。
    TextInput,
    /// 使用真实鼠标键盘输入作为 UIA 回退。
    SyntheticInput,
}

/// 图形 Provider 的生命周期状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualSessionState {
    /// Provider 在线且可以观察。
    Ready,
    /// 正在等待用户 Session。
    NoInteractiveDesktop,
    /// 已停止或失效。
    Stopped,
}

/// Provider 的显示器和缩放信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisualDisplay {
    pub display_id: String,
    pub physical_width: u32,
    pub physical_height: u32,
    pub logical_width: u32,
    pub logical_height: u32,
    pub dpi: u32,
    pub scale_percent: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

/// 远程桌面窗口的稳定指纹。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisualWindow {
    pub window_id: String,
    pub process_id: u32,
    pub process_name: String,
    pub title: String,
    pub automation_id: Option<String>,
    pub session_id: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub fingerprint: String,
}

/// UIA 控件或坐标目标。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VisualTarget {
    /// 通过 UIA 语义属性定位控件。
    Control {
        window_fingerprint: String,
        automation_id: Option<String>,
        name: Option<String>,
        control_type: Option<String>,
        target_fingerprint: String,
    },
    /// 经过显示器布局和截图缩放校验的坐标。
    Coordinate {
        window_fingerprint: String,
        display_id: String,
        x: i32,
        y: i32,
        screenshot_scale_percent: u32,
    },
}

/// 一次图形观察结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisualObservation {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub provider_instance_id: String,
    pub state: VisualSessionState,
    pub windows: Vec<VisualWindow>,
    pub displays: Vec<VisualDisplay>,
    pub active_window_fingerprint: Option<String>,
    pub ui_tree: Option<serde_json::Value>,
    pub screenshot_base64: Option<String>,
    pub screenshot_width: Option<u32>,
    pub screenshot_height: Option<u32>,
    pub redacted: bool,
}

/// 图形动作的结果，明确区分发送和效果验证。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisualActionResult {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub action_sent: bool,
    pub effect_verified: bool,
    pub observation: Option<VisualObservation>,
    pub error_code: Option<String>,
    pub message: String,
}
