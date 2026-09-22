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
    /// 物理屏幕坐标中的显示器原点，用于 DPI 坐标映射。
    #[serde(default)]
    pub physical_origin_x: i32,
    /// 物理屏幕坐标中的显示器原点，用于 DPI 坐标映射。
    #[serde(default)]
    pub physical_origin_y: i32,
}

impl VisualDisplay {
    /// 判断逻辑坐标是否落在当前显示器的半开区间内。
    #[must_use]
    pub fn contains_logical_point(&self, x: i32, y: i32) -> bool {
        let right = i64::from(self.origin_x) + i64::from(self.logical_width);
        let bottom = i64::from(self.origin_y) + i64::from(self.logical_height);
        i64::from(x) >= i64::from(self.origin_x)
            && i64::from(y) >= i64::from(self.origin_y)
            && i64::from(x) < right
            && i64::from(y) < bottom
    }
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
        /// 可选拖拽终点；旧请求缺少该字段时按普通单点目标处理。
        #[serde(default)]
        end_x: Option<i32>,
        /// 可选拖拽终点；必须与 `end_x` 同时提供。
        #[serde(default)]
        end_y: Option<i32>,
    },
}

impl VisualTarget {
    /// 返回坐标目标的起点和拖拽终点。
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn coordinate_points(&self) -> Option<((i32, i32), Option<(i32, i32)>)> {
        let Self::Coordinate {
            x, y, end_x, end_y, ..
        } = self
        else {
            return None;
        };
        Some(((*x, *y), end_x.zip(*end_y)))
    }
    /// 判断坐标目标是否描述了完整拖拽终点。
    #[must_use]
    pub fn is_drag_target(&self) -> bool {
        matches!(
            self,
            Self::Coordinate {
                end_x: Some(_),
                end_y: Some(_),
                ..
            }
        )
    }
    /// 校验坐标目标是否落在显示器逻辑边界。
    #[must_use]
    pub fn is_valid_for_display(&self, display: &VisualDisplay) -> bool {
        let Self::Coordinate {
            display_id,
            x,
            y,
            screenshot_scale_percent,
            end_x,
            end_y,
            ..
        } = self
        else {
            return false;
        };
        if display_id != &display.display_id
            || *screenshot_scale_percent == 0
            || !display.contains_logical_point(*x, *y)
        {
            return false;
        }
        match (end_x, end_y) {
            (None, None) => true,
            (Some(x), Some(y)) => display.contains_logical_point(*x, *y),
            _ => false,
        }
    }
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
    #[serde(default)]
    pub cursor_x: Option<i32>,
    #[serde(default)]
    pub cursor_y: Option<i32>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> VisualDisplay {
        VisualDisplay {
            display_id: "display-0".to_owned(),
            physical_width: 800,
            physical_height: 600,
            logical_width: 400,
            logical_height: 300,
            dpi: 144,
            scale_percent: 150,
            origin_x: -100,
            origin_y: 20,
            physical_origin_x: -150,
            physical_origin_y: 30,
        }
    }

    #[test]
    fn legacy_coordinate_json_defaults_drag_endpoint_to_none() {
        let target = serde_json::from_str::<VisualTarget>(
            r#"{"kind":"coordinate","window_fingerprint":"window-1","display_id":"display-0","x":0,"y":20,"screenshot_scale_percent":150}"#,
        )
        .expect("旧坐标目标 JSON 应保持兼容");

        assert_eq!(target.coordinate_points(), Some(((0, 20), None)));
        assert!(!target.is_drag_target());
        assert!(target.is_valid_for_display(&display()));
    }

    #[test]
    fn coordinate_drag_round_trips_and_requires_complete_endpoint() {
        let target = VisualTarget::Coordinate {
            window_fingerprint: "window-1".to_owned(),
            display_id: "display-0".to_owned(),
            x: 0,
            y: 20,
            screenshot_scale_percent: 150,
            end_x: Some(299),
            end_y: Some(319),
        };
        let json = serde_json::to_string(&target).expect("拖拽坐标目标应能序列化");
        let restored =
            serde_json::from_str::<VisualTarget>(&json).expect("拖拽坐标目标应能反序列化");

        assert_eq!(restored, target);
        assert!(json.contains("\"end_x\":299"));
        assert!(json.contains("\"end_y\":319"));
        assert_eq!(
            restored.coordinate_points(),
            Some(((0, 20), Some((299, 319))))
        );
        assert!(restored.is_drag_target());
        assert!(restored.is_valid_for_display(&display()));

        let incomplete = VisualTarget::Coordinate {
            window_fingerprint: "window-1".to_owned(),
            display_id: "display-0".to_owned(),
            x: 0,
            y: 20,
            screenshot_scale_percent: 150,
            end_x: Some(1),
            end_y: None,
        };
        assert!(!incomplete.is_drag_target());
        assert!(!incomplete.is_valid_for_display(&display()));
    }

    #[test]
    fn coordinate_validation_uses_negative_origin_and_exclusive_edges() {
        let display = display();
        let valid = VisualTarget::Coordinate {
            window_fingerprint: "window-1".to_owned(),
            display_id: "display-0".to_owned(),
            x: -100,
            y: 20,
            screenshot_scale_percent: 100,
            end_x: Some(299),
            end_y: Some(319),
        };
        assert!(valid.is_valid_for_display(&display));

        for (x, y) in [(300, 20), (0, 320), (-101, 20), (-100, 19)] {
            let out_of_bounds = VisualTarget::Coordinate {
                window_fingerprint: "window-1".to_owned(),
                display_id: "display-0".to_owned(),
                x,
                y,
                screenshot_scale_percent: 100,
                end_x: Some(299),
                end_y: Some(319),
            };
            assert!(
                !out_of_bounds.is_valid_for_display(&display),
                "越界点 ({x}, {y}) 应拒绝"
            );
        }

        let zero_scale = VisualTarget::Coordinate {
            window_fingerprint: "window-1".to_owned(),
            display_id: "display-0".to_owned(),
            x: -100,
            y: 20,
            screenshot_scale_percent: 0,
            end_x: Some(299),
            end_y: Some(319),
        };
        assert!(!zero_scale.is_valid_for_display(&display));
    }
}
