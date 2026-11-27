use thiserror::Error;

/// 领域层稳定错误。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DomainError {
    /// UUID 或其他内部标识格式无效。
    #[error("标识格式无效：{0}")]
    InvalidIdentifier(String),
    /// 临时配对码的字符或长度不合法。
    #[error("短配对码须为九位字母和数字，或九位纯数字")]
    InvalidPairingCode,
    /// 目标连接不存在。
    #[error("未找到目标连接")]
    ConnectionNotFound,
    /// 别名或搜索条件匹配了多个连接。
    #[error("目标连接存在歧义：{0}")]
    AmbiguousTarget(String),
    /// 别名为空或不合法。
    #[error("连接别名不能为空")]
    InvalidAlias,
    /// 会话状态不允许当前操作。
    #[error("当前会话状态不允许该操作：{0}")]
    InvalidSessionState(String),
}
