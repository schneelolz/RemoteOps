use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentInstanceId, PairingCode};

/// Relay 为 Agent 颁发的配对码租约。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PairingLease {
    /// Agent 实例标识。
    pub agent_instance_id: AgentInstanceId,
    /// 租约有效期内保持不变的配对码。
    pub pairing_code: PairingCode,
    /// 租约到期时间。
    pub expires_at: DateTime<Utc>,
}

impl PairingLease {
    /// 创建新租约。
    #[must_use]
    pub fn new(
        agent_instance_id: AgentInstanceId,
        pairing_code: PairingCode,
        now: DateTime<Utc>,
        lifetime: Duration,
    ) -> Self {
        Self {
            agent_instance_id,
            pairing_code,
            expires_at: now + lifetime,
        }
    }

    /// 续租但不改变配对码。
    pub fn renew(&mut self, now: DateTime<Utc>, lifetime: Duration) {
        self.expires_at = now + lifetime;
    }

    /// 判断租约在指定时间是否有效。
    #[must_use]
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renew_keeps_pairing_code_stable() {
        let now = Utc::now();
        let code = PairingCode::parse("123456789").expect("测试配对码应有效");
        let mut lease = PairingLease::new(
            AgentInstanceId::new(),
            code.clone(),
            now,
            Duration::minutes(5),
        );

        lease.renew(now + Duration::minutes(4), Duration::minutes(5));

        assert_eq!(lease.pairing_code, code);
        assert!(lease.is_valid_at(now + Duration::minutes(8)));
        assert!(!lease.is_valid_at(now + Duration::minutes(9)));
    }
}

