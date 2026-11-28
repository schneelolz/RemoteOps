//! 中继工具中心的独立存储、包验证和共享管理契约。
//! 权限由已认证的中继连接检查；本层始终隔离操作者的上传及幂等请求。

mod model;
mod packages;
mod store;

pub use model::*;
pub use store::ToolCenter;

/// 工具中心的脱敏结果。
pub type Result<T> = std::result::Result<T, ToolCenterError>;

pub(crate) fn error(code: &str, message: &str) -> ToolCenterError {
    ToolCenterError {
        code: code.into(),
        message: message.into(),
    }
}

pub(crate) fn storage(_: impl std::fmt::Display) -> ToolCenterError {
    error("tool_storage_unavailable", "工具仓库读写失败")
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c))
}

pub(crate) fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[cfg(test)]
mod tests;
