use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{DomainError, PairingCode};

/// 经校验的短控制码或随机 UUID v4 长授权码。
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccessCode(String);

impl AccessCode {
    /// 校验九位码或标准 UUID v4，不接受设备标识作为其他格式的授权码。
    /// # Errors
    /// 非法输入返回控制码错误。
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if let Ok(code) = PairingCode::parse(value.clone()) {
            return Ok(Self(code.as_str().to_owned()));
        }
        let value = value.trim();
        let uuid = Uuid::parse_str(value).map_err(|_| DomainError::InvalidPairingCode)?;
        if value.len() != 36
            || uuid.get_version_num() != 4
            || uuid.get_variant() != uuid::Variant::RFC4122
        {
            return Err(DomainError::InvalidPairingCode);
        }
        Ok(Self(uuid.hyphenated().to_string()))
    }

    /// 创建密码学随机长授权码；由 Relay 调用签发。
    #[must_use]
    pub fn random_long() -> Self {
        Self(Uuid::new_v4().hyphenated().to_string())
    }

    /// 返回规范化凭据，仅用于协议和受保护状态存储。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为长授权码。
    #[must_use]
    pub fn is_long(&self) -> bool {
        self.0.len() == 36
    }

    /// 是否为需新版 Agent 才能接收的字母数字短码。
    #[must_use]
    pub fn is_mixed_short(&self) -> bool {
        !self.is_long() && self.0.bytes().any(|byte| byte.is_ascii_alphabetic())
    }

    /// 短码分组，长码保留标准格式。
    #[must_use]
    pub fn display_grouped(&self) -> String {
        if self.is_long() {
            self.0.clone()
        } else {
            format!("{}-{}-{}", &self.0[..3], &self.0[3..6], &self.0[6..])
        }
    }
}

impl fmt::Debug for AccessCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessCode([REDACTED])")
    }
}
impl fmt::Display for AccessCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display_grouped())
    }
}
impl TryFrom<String> for AccessCode {
    type Error = DomainError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}
impl From<AccessCode> for String {
    fn from(value: AccessCode) -> Self {
        value.0
    }
}
impl From<PairingCode> for AccessCode {
    fn from(value: PairingCode) -> Self {
        Self(value.as_str().to_owned())
    }
}
impl FromStr for AccessCode {
    type Err = DomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_and_redacts_both_formats() {
        let short = AccessCode::parse("123-456-789").unwrap();
        assert_eq!(short.as_str(), "123456789");
        let long = AccessCode::random_long();
        assert_eq!(
            AccessCode::parse(long.as_str().to_uppercase()).unwrap(),
            long
        );
        assert!(!format!("{long:?}").contains(long.as_str()));
        assert!(PairingCode::parse(long.as_str()).is_err());
        assert!(AccessCode::parse(Uuid::nil().to_string()).is_err());
        assert!(serde_json::from_str::<AccessCode>("\"invalid\"").is_err());
    }
}
