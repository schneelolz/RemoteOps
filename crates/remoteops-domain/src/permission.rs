use serde::{Deserialize, Serialize};

/// Agent 会话使用的统一权限模式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// 只允许经过策略确认的只读操作。
    ReadOnly,
    /// 读取自动执行，修改和高风险操作需要人工逐项审批。
    #[default]
    ApprovalRequired,
    /// 由可信本地 Controller 完成人机确认，Relay 和 Agent 继续执行结构化安全校验。
    ControllerApproved,
    /// 已认证 Owner 显式授权后，操作不再逐项审批。
    FullAccess,
}

