//! 图形状态等待条件的共享解析与匹配逻辑。
//!
//! MCP 和 Agent 都只传递 JSON 字符串，因此这里集中定义受支持的条件格式，
//! 避免两个边界对同一条件采用不同解释。

use crate::{VisualObservation, VisualSessionState};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 默认等待时长（毫秒）。
pub const DEFAULT_VISUAL_WAIT_TIMEOUT_MILLIS: u64 = 30_000;
/// 单次等待允许的最大时长（毫秒）。
pub const MAX_VISUAL_WAIT_TIMEOUT_MILLIS: u64 = 120_000;
/// 条件 JSON 的最大长度，避免把等待接口当成大载荷通道。
pub const MAX_VISUAL_WAIT_CONDITION_BYTES: usize = 64 * 1024;

/// 解析并限制图形等待超时；零值无意义，超过上限时按上限执行。
///
/// # Errors
/// 当显式传入零值时返回错误。
pub fn normalize_visual_wait_timeout(timeout_millis: Option<u64>) -> Result<u64, String> {
    let timeout = timeout_millis.unwrap_or(DEFAULT_VISUAL_WAIT_TIMEOUT_MILLIS);
    if timeout == 0 {
        return Err("图形等待 timeout_millis 必须大于 0".to_owned());
    }
    Ok(timeout.min(MAX_VISUAL_WAIT_TIMEOUT_MILLIS))
}

/// 支持的图形等待条件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VisualWaitCondition {
    /// 等待窗口出现或消失。
    Window {
        fingerprint: Option<String>,
        title: Option<String>,
        process_name: Option<String>,
        exists: bool,
        case_sensitive: bool,
    },
    /// 等待 UIA 树或窗口摘要中出现文本。
    Text {
        text: String,
        contains: bool,
        case_sensitive: bool,
    },
    /// 等待 UIA 控件出现或消失。
    Control {
        target_fingerprint: Option<String>,
        automation_id: Option<String>,
        name: Option<String>,
        control_type: Option<String>,
        exists: bool,
        case_sensitive: bool,
    },
    /// 等待当前截图的 SHA-256 指纹。
    Hash { sha256: String },
}

#[derive(Debug, Deserialize)]
struct RawWaitCondition {
    #[serde(default)]
    kind: Option<String>,
    #[serde(rename = "type", default)]
    condition_type: Option<String>,
    fingerprint: Option<String>,
    #[serde(alias = "window_fingerprint")]
    window_fingerprint: Option<String>,
    title: Option<String>,
    process_name: Option<String>,
    text: Option<String>,
    #[serde(default = "default_true")]
    contains: bool,
    #[serde(default = "default_true")]
    exists: bool,
    #[serde(default)]
    case_sensitive: bool,
    target_fingerprint: Option<String>,
    automation_id: Option<String>,
    name: Option<String>,
    control_type: Option<String>,
    #[serde(alias = "hash")]
    sha256: Option<String>,
}

const fn default_true() -> bool {
    true
}

impl VisualWaitCondition {
    /// 从 MCP/协议传入的 JSON 字符串解析等待条件。
    ///
    /// 支持的格式示例：
    /// `{"kind":"window","title":"记事本"}`、
    /// `{"kind":"text","text":"完成"}`、
    /// `{"kind":"control","automation_id":"ok"}`、
    /// `{"kind":"hash","sha256":"..."}`。
    ///
    /// # Errors
    /// JSON 无效、条件类型不支持或必填匹配字段为空时返回错误。
    pub fn parse(condition: &str) -> Result<Self, String> {
        if condition.len() > MAX_VISUAL_WAIT_CONDITION_BYTES {
            return Err("图形等待 condition 超出 64 KiB 限制".to_owned());
        }
        let raw: RawWaitCondition = serde_json::from_str(condition)
            .map_err(|error| format!("图形等待 condition 必须是有效 JSON：{error}"))?;
        let kind = raw
            .kind
            .or(raw.condition_type)
            .ok_or_else(|| "图形等待 condition 缺少 kind".to_owned())?
            .trim()
            .to_ascii_lowercase();
        match kind.as_str() {
            "window" => {
                let fingerprint = non_empty(raw.fingerprint.or(raw.window_fingerprint));
                let title = non_empty(raw.title);
                let process_name = non_empty(raw.process_name);
                if fingerprint.is_none() && title.is_none() && process_name.is_none() {
                    return Err(
                        "window 等待条件至少需要 fingerprint、title 或 process_name".to_owned()
                    );
                }
                Ok(Self::Window {
                    fingerprint,
                    title,
                    process_name,
                    exists: raw.exists,
                    case_sensitive: raw.case_sensitive,
                })
            }
            "text" => {
                let text = raw
                    .text
                    .ok_or_else(|| "text 等待条件缺少 text".to_owned())?;
                let text = text.trim().to_owned();
                if text.is_empty() {
                    return Err("text 等待条件的 text 不能为空".to_owned());
                }
                if text.chars().count() > 4096 {
                    return Err("text 等待条件的 text 超出 4096 字符限制".to_owned());
                }
                Ok(Self::Text {
                    text,
                    contains: raw.contains,
                    case_sensitive: raw.case_sensitive,
                })
            }
            "control" => {
                let target_fingerprint = non_empty(raw.target_fingerprint);
                let automation_id = non_empty(raw.automation_id);
                let name = non_empty(raw.name);
                let control_type = non_empty(raw.control_type);
                if target_fingerprint.is_none()
                    && automation_id.is_none()
                    && name.is_none()
                    && control_type.is_none()
                {
                    return Err("control 等待条件至少需要 target_fingerprint、automation_id、name 或 control_type".to_owned());
                }
                Ok(Self::Control {
                    target_fingerprint,
                    automation_id,
                    name,
                    control_type,
                    exists: raw.exists,
                    case_sensitive: raw.case_sensitive,
                })
            }
            "hash" | "screenshot_hash" => {
                let sha256 = raw
                    .sha256
                    .ok_or_else(|| "hash 等待条件缺少 sha256".to_owned())?
                    .trim()
                    .to_ascii_lowercase();
                if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err("hash 等待条件的 sha256 必须是 64 位十六进制字符串".to_owned());
                }
                Ok(Self::Hash { sha256 })
            }
            other => Err(format!("不支持的图形等待条件类型：{other}")),
        }
    }

    /// 判断一次观察是否满足条件。
    #[must_use]
    pub fn matches(&self, observation: &VisualObservation) -> bool {
        if observation.state != VisualSessionState::Ready {
            return false;
        }
        match self {
            Self::Window {
                fingerprint,
                title,
                process_name,
                exists,
                case_sensitive,
            } => {
                let found = observation.windows.iter().any(|window| {
                    fingerprint.as_deref().is_some_and(|expected| {
                        equal(expected, &window.fingerprint, *case_sensitive)
                    }) || title
                        .as_deref()
                        .is_some_and(|expected| equal(expected, &window.title, *case_sensitive))
                        || process_name.as_deref().is_some_and(|expected| {
                            equal(expected, &window.process_name, *case_sensitive)
                        })
                });
                found == *exists
            }
            Self::Text {
                text,
                contains,
                case_sensitive,
            } => observation
                .ui_tree
                .as_ref()
                .is_some_and(|tree| text_in_json(tree, text, *contains, *case_sensitive)),
            Self::Control {
                target_fingerprint,
                automation_id,
                name,
                control_type,
                exists,
                case_sensitive,
            } => {
                let found = observation.ui_tree.as_ref().is_some_and(|tree| {
                    control_in_json(
                        tree,
                        target_fingerprint.as_deref(),
                        automation_id.as_deref(),
                        name.as_deref(),
                        control_type.as_deref(),
                        *case_sensitive,
                    )
                });
                found == *exists
            }
            Self::Hash { sha256 } => screenshot_sha256(observation)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(sha256)),
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    })
}

fn equal(expected: &str, actual: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        expected == actual
    } else {
        expected.eq_ignore_ascii_case(actual)
    }
}

fn text_in_json(
    value: &serde_json::Value,
    expected: &str,
    contains: bool,
    case_sensitive: bool,
) -> bool {
    match value {
        serde_json::Value::String(actual) => {
            if case_sensitive {
                if contains {
                    actual.contains(expected)
                } else {
                    actual == expected
                }
            } else {
                let actual = actual.to_ascii_lowercase();
                let expected = expected.to_ascii_lowercase();
                if contains {
                    actual.contains(&expected)
                } else {
                    actual == expected
                }
            }
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| text_in_json(value, expected, contains, case_sensitive)),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| text_in_json(value, expected, contains, case_sensitive)),
        _ => false,
    }
}

fn control_in_json(
    value: &serde_json::Value,
    target_fingerprint: Option<&str>,
    automation_id: Option<&str>,
    name: Option<&str>,
    control_type: Option<&str>,
    case_sensitive: bool,
) -> bool {
    match value {
        serde_json::Value::Object(values) => {
            let field = |key: &str| values.get(key).and_then(serde_json::Value::as_str);
            let matches = target_fingerprint.is_none_or(|expected| {
                field("target_fingerprint")
                    .or_else(|| field("fingerprint"))
                    .is_some_and(|actual| equal(expected, actual, case_sensitive))
            }) && automation_id.is_none_or(|expected| {
                field("automation_id").is_some_and(|actual| equal(expected, actual, case_sensitive))
            }) && name.is_none_or(|expected| {
                field("name").is_some_and(|actual| equal(expected, actual, case_sensitive))
            }) && control_type.is_none_or(|expected| {
                field("control_type")
                    .or_else(|| field("controlType"))
                    .is_some_and(|actual| equal(expected, actual, case_sensitive))
            });
            matches
                || values.values().any(|value| {
                    control_in_json(
                        value,
                        target_fingerprint,
                        automation_id,
                        name,
                        control_type,
                        case_sensitive,
                    )
                })
        }
        serde_json::Value::Array(values) => values.iter().any(|value| {
            control_in_json(
                value,
                target_fingerprint,
                automation_id,
                name,
                control_type,
                case_sensitive,
            )
        }),
        _ => false,
    }
}

fn screenshot_sha256(observation: &VisualObservation) -> Option<String> {
    if let Some(value) = observation
        .ui_tree
        .as_ref()
        .and_then(|tree| tree.get("screenshot_sha256"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(value.to_ascii_lowercase());
    }
    let encoded = observation.screenshot_base64.as_deref()?;
    let bytes = BASE64.decode(encoded).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestId, SessionId, VisualWindow};

    fn ready_observation() -> VisualObservation {
        VisualObservation {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            provider_instance_id: "test".to_owned(),
            state: VisualSessionState::Ready,
            windows: vec![VisualWindow {
                window_id: "1".to_owned(),
                process_id: 1,
                process_name: "notepad.exe".to_owned(),
                title: "Notes".to_owned(),
                automation_id: None,
                session_id: "7".to_owned(),
                left: 0,
                top: 0,
                width: 200,
                height: 100,
                fingerprint: "window-fingerprint".to_owned(),
            }],
            displays: Vec::new(),
            active_window_fingerprint: Some("window-fingerprint".to_owned()),
            ui_tree: Some(serde_json::json!({
                "name":"Root",
                "children":[{"name":"完成", "automation_id":"ok", "control_type":"Button", "target_fingerprint":"control-fingerprint"}]
            })),
            screenshot_base64: Some(BASE64.encode(b"pixels")),
            screenshot_width: Some(1),
            screenshot_height: Some(1),
            cursor_x: None,
            cursor_y: None,
            redacted: false,
        }
    }

    #[test]
    fn parses_supported_conditions_and_rejects_ambiguous_input() {
        assert!(matches!(
            VisualWaitCondition::parse(r#"{"kind":"window","title":"Notes"}"#),
            Ok(VisualWaitCondition::Window { .. })
        ));
        assert!(matches!(
            VisualWaitCondition::parse(r#"{"type":"text","text":"完成"}"#),
            Ok(VisualWaitCondition::Text { .. })
        ));
        assert!(VisualWaitCondition::parse(r#"{"kind":"control","automation_id":"ok"}"#).is_ok());
        assert!(
            VisualWaitCondition::parse(&format!(
                r#"{{"kind":"hash","sha256":"{}"}}"#,
                "a".repeat(64)
            ))
            .is_ok()
        );
        assert!(VisualWaitCondition::parse(r#"{"kind":"window"}"#).is_err());
        assert!(VisualWaitCondition::parse(r#"{"kind":"unknown","text":"x"}"#).is_err());
    }

    #[test]
    fn matches_window_text_control_and_hash_conditions() {
        let observation = ready_observation();
        assert!(
            VisualWaitCondition::parse(r#"{"kind":"window","title":"notes"}"#)
                .unwrap()
                .matches(&observation)
        );
        assert!(
            VisualWaitCondition::parse(r#"{"kind":"text","text":"完成"}"#)
                .unwrap()
                .matches(&observation)
        );
        assert!(
            VisualWaitCondition::parse(
                r#"{"kind":"control","target_fingerprint":"control-fingerprint"}"#
            )
            .unwrap()
            .matches(&observation)
        );
        let digest = format!("{:x}", Sha256::digest(b"pixels"));
        assert!(
            VisualWaitCondition::parse(&format!(r#"{{"kind":"hash","sha256":"{digest}"}}"#))
                .unwrap()
                .matches(&observation)
        );
        assert!(
            !VisualWaitCondition::parse(r#"{"kind":"text","text":"完成x","contains":false}"#)
                .unwrap()
                .matches(&observation)
        );
    }

    #[test]
    fn normalizes_timeout_without_allowing_zero_or_unbounded_waits() {
        assert_eq!(
            normalize_visual_wait_timeout(None).unwrap(),
            DEFAULT_VISUAL_WAIT_TIMEOUT_MILLIS
        );
        assert_eq!(
            normalize_visual_wait_timeout(Some(999_999)).unwrap(),
            MAX_VISUAL_WAIT_TIMEOUT_MILLIS
        );
        assert!(normalize_visual_wait_timeout(Some(0)).is_err());
    }
}
