use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DomainError;

macro_rules! uuid_identifier {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// 创建新的随机标识。
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// 从既有 UUID 创建标识。
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// 取得底层 UUID。
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value)
                    .map(Self)
                    .map_err(|_| DomainError::InvalidIdentifier(value.to_owned()))
            }
        }
    };
}

uuid_identifier!(
    /// Agent 进程生命周期内保持不变的实例标识。
    AgentInstanceId
);
uuid_identifier!(
    /// 控制端进程实例标识。
    ControllerInstanceId
);
uuid_identifier!(
    /// Human 与 AI Controller 共同归属的稳定所有者标识。
    ControllerOwnerId
);
uuid_identifier!(
    /// 控制端与 Agent 之间不可变的逻辑会话标识。
    SessionId
);
uuid_identifier!(
    /// 一次远程请求的关联标识。
    RequestId
);
uuid_identifier!(
    /// 一次审批请求的标识。
    ApprovalId
);
uuid_identifier!(
    /// 交互式 Shell 会话标识。
    ShellId
);
uuid_identifier!(
    /// 串口会话标识。
    SerialSessionId
);
uuid_identifier!(
    /// 一次分块文件传输的标识。
    FileTransferId
);

/// 人工输入的临时配对码。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PairingCode(String);

impl PairingCode {
    /// 创建并校验配对码。
    ///
    /// # Errors
    ///
    /// 当控制码不是九位数字或分组格式不正确时返回错误。
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let compact = value.replace([' ', '-'], "");
        if compact.len() != 9 || !compact.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DomainError::InvalidPairingCode);
        }
        Ok(Self(compact))
    }

    /// 返回不含分隔符的配对码。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 返回便于人工抄写的分组格式。
    #[must_use]
    pub fn display_grouped(&self) -> String {
        format!("{}-{}-{}", &self.0[0..3], &self.0[3..6], &self.0[6..9])
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display_grouped())
    }
}

impl FromStr for PairingCode {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_accepts_grouped_and_compact_forms() {
        let grouped = PairingCode::parse("123-456-789").expect("分组配对码应有效");
        let compact = PairingCode::parse("123456789").expect("紧凑配对码应有效");

        assert_eq!(grouped, compact);
        assert_eq!(grouped.as_str(), "123456789");
        assert_eq!(grouped.to_string(), "123-456-789");
    }

    #[test]
    fn pairing_code_rejects_invalid_values() {
        assert!(PairingCode::parse("123").is_err());
        assert!(PairingCode::parse("12345678x").is_err());
    }
}
