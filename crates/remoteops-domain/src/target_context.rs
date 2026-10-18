//! 按目标会话传递的产品说明和运行事实；这些信息不授予任何执行权限。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 当前目标上下文结构版本。
pub const TARGET_CONTEXT_SCHEMA_VERSION: u16 = 1;
/// 目标上下文编码后的最大长度。
pub const MAX_TARGET_CONTEXT_BYTES: usize = 32 * 1024;

/// Agent 宿主的真实运行模式，未知值不推断为交互宿主。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunMode {
    /// Windows SCM 服务宿主。
    WindowsService,
    /// 交互式桌面宿主。
    Interactive,
    /// 前台命令行宿主。
    Console,
    /// 未报告或当前组件无法识别的模式。
    #[default]
    #[serde(other)]
    Unknown,
}

/// 能力或运行事实的三态结果；未知不代表可用。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    /// 已由当前运行时明确确认可用。
    Available,
    /// 已确认不可用或被产品策略禁用。
    Unavailable,
    /// 未采集或当前组件无法识别。
    #[default]
    #[serde(other)]
    Unknown,
}

/// 宿主采集的事实，不通过截图或产品自由文本推断。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetRuntimeFacts {
    /// 当前进程所在的 Windows 会话；无法可靠判断时为空。
    pub process_session_id: Option<u32>,
    /// 当前宿主是否有可供本 Agent 操作的交互桌面。
    pub interactive_desktop: CapabilityAvailability,
    /// 本组事实的采集时间。
    pub observed_at: DateTime<Utc>,
    /// 事实来源，如 `host_runtime`；自由文本不视为证明。
    pub source: String,
}

impl Default for TargetRuntimeFacts {
    fn default() -> Self {
        Self {
            process_session_id: None,
            interactive_desktop: CapabilityAvailability::Unknown,
            observed_at: DateTime::<Utc>::UNIX_EPOCH,
            source: "unknown".to_owned(),
        }
    }
}

/// 一项有效能力及其稳定的禁用原因。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetCapabilityStatus {
    /// 稳定能力名称；未知名称只用于展示。
    pub capability: String,
    /// 经过产品限制和运行时事实合并后的状态。
    pub status: CapabilityAvailability,
    /// 稳定原因码，未知码保留原文且不推断为允许。
    pub reasons: Vec<String>,
}

/// 产品案例库或说明文档的引用，不自动访问或执行。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuidanceReference {
    /// 引用在产品中的稳定标识。
    pub id: String,
    /// 内容版本。
    pub version: String,
    /// 发布来源，不代表已通过厂商签名验证。
    pub source: String,
    /// 文档位置，仅作为参考数据返回。
    pub uri: String,
}

/// 当前目标专用的辅助说明，不能覆盖用户授权或公共安全策略。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetGuidance {
    /// 指引的稳定标识。
    pub id: String,
    /// 指引内容版本。
    pub version: String,
    /// 内容来源，如 `built_in_product` 或 `admin_config`。
    pub source: String,
    /// 最多 16 KiB 的辅助说明正文。
    pub text: String,
    /// 最多八条文档或案例库引用。
    pub references: Vec<GuidanceReference>,
}

/// 随 Agent 的当前启动和目标会话传播的标准上下文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetContext {
    /// 上下文结构版本；未支持的版本不用于能力判定。
    #[serde(default = "unknown_schema_version")]
    pub schema_version: u16,
    /// 当前产品的稳定标识。
    pub product_id: String,
    /// 产品显示名称。
    pub name: String,
    /// 产品版本。
    pub version: String,
    /// 真实宿主运行模式。
    pub run_mode: AgentRunMode,
    /// 宿主采集的运行事实。
    pub facts: TargetRuntimeFacts,
    /// 产品限制与运行事实合并后的能力。
    pub capabilities: Vec<TargetCapabilityStatus>,
    /// 仅属于该目标的辅助说明。
    pub guidance: Vec<TargetGuidance>,
}

fn unknown_schema_version() -> u16 {
    0
}

impl Default for TargetContext {
    fn default() -> Self {
        Self {
            schema_version: TARGET_CONTEXT_SCHEMA_VERSION,
            product_id: String::new(),
            name: String::new(),
            version: String::new(),
            run_mode: AgentRunMode::Unknown,
            facts: TargetRuntimeFacts::default(),
            capabilities: Vec::new(),
            guidance: Vec::new(),
        }
    }
}

impl TargetContext {
    /// 校验大小、版本和重复能力；失败时不能依赖上下文放行操作。
    ///
    /// # Errors
    /// 返回不包含机密的校验原因。
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != TARGET_CONTEXT_SCHEMA_VERSION {
            return Err("unsupported_target_context_version".to_owned());
        }
        if self.product_id.len() > 64
            || !self
                .product_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            || self.name.len() > 256
            || self.version.len() > 64
            || self.facts.source.len() > 128
        {
            return Err("invalid_target_identity".to_owned());
        }
        if self.capabilities.len() > 64 || self.guidance.len() > 8 {
            return Err("target_context_limit_exceeded".to_owned());
        }
        let mut seen = std::collections::BTreeSet::new();
        for capability in &self.capabilities {
            if capability.capability.is_empty()
                || capability.capability.len() > 64
                || !seen.insert(&capability.capability)
                || capability.reasons.len() > 8
                || capability.reasons.iter().any(|reason| reason.len() > 128)
            {
                return Err("invalid_capability_status".to_owned());
            }
        }
        let mut total_text = 0;
        let mut total_references = 0;
        for guidance in &self.guidance {
            total_text += guidance.text.len();
            total_references += guidance.references.len();
            if guidance.id.is_empty()
                || guidance.id.len() > 128
                || guidance.version.is_empty()
                || guidance.version.len() > 64
                || guidance.source.is_empty()
                || guidance.source.len() > 128
            {
                return Err("invalid_guidance_provenance".to_owned());
            }
            for reference in &guidance.references {
                if reference.id.is_empty()
                    || reference.id.len() > 128
                    || reference.version.is_empty()
                    || reference.version.len() > 64
                    || reference.source.is_empty()
                    || reference.source.len() > 128
                    || reference.uri.is_empty()
                    || reference.uri.len() > 2048
                    || reference.uri.chars().any(char::is_control)
                {
                    return Err("invalid_guidance_reference".to_owned());
                }
            }
        }
        if total_text > 16 * 1024 || total_references > 8 {
            return Err("guidance_limit_exceeded".to_owned());
        }
        if serde_json::to_vec(self)
            .map_err(|_| "invalid_target_context".to_owned())?
            .len()
            > MAX_TARGET_CONTEXT_BYTES
        {
            return Err("target_context_limit_exceeded".to_owned());
        }
        Ok(())
    }

    /// 判断当前上下文是否明确允许使用图形 Provider；不授予写操作权限。
    #[must_use]
    pub fn visual_allowed(&self) -> bool {
        self.visual_disabled_reason().is_none()
    }

    /// 返回阻止图形操作的稳定原因；未知上下文不触发探测。
    #[must_use]
    pub fn visual_disabled_reason(&self) -> Option<&str> {
        if self.validate().is_err() || self.run_mode == AgentRunMode::Unknown {
            return Some("host_context_unknown");
        }
        if self.run_mode == AgentRunMode::WindowsService {
            return Some("product_policy_disabled");
        }
        let Some(status) = self
            .capabilities
            .iter()
            .find(|status| status.capability == "visual")
        else {
            return Some("capability_status_unknown");
        };
        if status.status != CapabilityAvailability::Available {
            return Some(
                status
                    .reasons
                    .first()
                    .map_or("capability_unavailable", String::as_str),
            );
        }
        if self.facts.observed_at <= DateTime::<Utc>::UNIX_EPOCH
            || self.facts.source.is_empty()
            || self.facts.source == "unknown"
        {
            return Some("host_context_unknown");
        }
        if self.facts.interactive_desktop != CapabilityAvailability::Available {
            return Some("no_interactive_desktop");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_or_service_context_never_enables_visual() {
        let mut context = TargetContext::default();
        assert!(!context.visual_allowed());
        context.run_mode = AgentRunMode::WindowsService;
        context.facts.interactive_desktop = CapabilityAvailability::Available;
        context.capabilities.push(TargetCapabilityStatus {
            capability: "visual".to_owned(),
            status: CapabilityAvailability::Available,
            reasons: vec![],
        });
        assert!(!context.visual_allowed());
        context.run_mode = AgentRunMode::Interactive;
        context.facts.source = "host_runtime".to_owned();
        context.facts.observed_at = Utc::now();
        assert!(context.visual_allowed());
        context.facts.interactive_desktop = CapabilityAvailability::Unknown;
        assert!(!context.visual_allowed());
    }
    #[test]
    fn unknown_enum_values_decode_to_unknown_without_granting_access() {
        let context: TargetContext = serde_json::from_value(serde_json::json!({"run_mode":"future_host","facts":{"interactive_desktop":"future_state"}})).unwrap();
        assert_eq!(context.run_mode, AgentRunMode::Unknown);
        assert_eq!(
            context.facts.interactive_desktop,
            CapabilityAvailability::Unknown
        );
        assert!(!context.visual_allowed());
    }
    #[test]
    fn context_limits_and_duplicate_capabilities_are_rejected() {
        let mut context = TargetContext::default();
        context.guidance.push(TargetGuidance {
            id: "guide".into(),
            source: "built_in_product".into(),
            version: "1".into(),
            text: "x".repeat(16 * 1024 + 1),
            ..TargetGuidance::default()
        });
        assert!(context.validate().is_err());
        context.guidance.clear();
        let capability = TargetCapabilityStatus {
            capability: "visual".into(),
            ..TargetCapabilityStatus::default()
        };
        context.capabilities = vec![capability.clone(), capability];
        assert!(context.validate().is_err());
    }
}
