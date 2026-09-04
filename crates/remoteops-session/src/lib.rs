//! 会话注册、目标解析和人工/AI 控制权。

use std::collections::BTreeMap;

use chrono::Utc;
use remoteops_domain::{
    ConnectionDescriptor, ConnectionState, DomainError, SessionId, SessionRole,
};

/// 控制端持有的多连接注册表。
#[derive(Clone, Debug, Default)]
pub struct ConnectionRegistry {
    connections: BTreeMap<SessionId, ConnectionDescriptor>,
    next_display_index: u32,
}

impl ConnectionRegistry {
    /// 创建空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self {
            connections: BTreeMap::new(),
            next_display_index: 1,
        }
    }

    /// 新增或更新连接；首次看到的连接会获得稳定显示序号。
    pub fn upsert(&mut self, mut connection: ConnectionDescriptor) -> ConnectionDescriptor {
        if let Some(existing) = self.connections.get(&connection.session_id) {
            connection.display_index = existing.display_index;
            if connection.alias.is_none() {
                connection.alias.clone_from(&existing.alias);
            }
        } else if let Some(previous_session_id) = self
            .connections
            .iter()
            .find(|(_, existing)| existing.agent_instance_id == connection.agent_instance_id)
            .map(|(session_id, _)| *session_id)
        {
            if let Some(existing) = self.connections.remove(&previous_session_id) {
                connection.display_index = existing.display_index;
                if connection.alias.is_none() {
                    connection.alias = existing.alias;
                }
            }
        } else {
            connection.display_index = self.next_display_index;
            self.next_display_index = self.next_display_index.saturating_add(1);
        }
        self.connections
            .insert(connection.session_id, connection.clone());
        connection
    }

    /// 返回所有连接，按照显示序号排序。
    #[must_use]
    pub fn list(&self) -> Vec<ConnectionDescriptor> {
        let mut values: Vec<_> = self.connections.values().cloned().collect();
        values.sort_by_key(|connection| connection.display_index);
        values
    }

    /// 按会话标识取得连接。
    #[must_use]
    pub fn get(&self, session_id: SessionId) -> Option<&ConnectionDescriptor> {
        self.connections.get(&session_id)
    }

    /// 从本地注册表移除连接。
    ///
    /// # Errors
    ///
    /// 当指定会话不存在时返回错误。
    pub fn remove(&mut self, session_id: SessionId) -> Result<ConnectionDescriptor, DomainError> {
        self.connections
            .remove(&session_id)
            .ok_or(DomainError::ConnectionNotFound)
    }

    /// 修改连接别名。
    ///
    /// # Errors
    ///
    /// 当别名为空或指定会话不存在时返回错误。
    pub fn set_alias(
        &mut self,
        session_id: SessionId,
        alias: impl Into<String>,
    ) -> Result<(), DomainError> {
        let alias = alias.into().trim().to_owned();
        if alias.is_empty() {
            return Err(DomainError::InvalidAlias);
        }
        let connection = self
            .connections
            .get_mut(&session_id)
            .ok_or(DomainError::ConnectionNotFound)?;
        connection.alias = Some(alias);
        connection.updated_at = Utc::now();
        Ok(())
    }

    /// 使用会话 UUID、默认编号或别名解析唯一目标。
    ///
    /// # Errors
    ///
    /// 当目标不存在或别名匹配到多个连接时返回错误。
    pub fn resolve(&self, target: &str) -> Result<SessionId, DomainError> {
        let target = target.trim();
        if let Ok(session_id) = target.parse::<SessionId>()
            && self.connections.contains_key(&session_id)
        {
            return Ok(session_id);
        }

        let normalized = target.to_lowercase();
        let candidates: Vec<_> = self
            .connections
            .values()
            .filter(|connection| {
                let default_name = format!("连接 {}", connection.display_index);
                default_name.to_lowercase() == normalized
                    || connection
                        .alias
                        .as_ref()
                        .is_some_and(|alias| alias.to_lowercase() == normalized)
            })
            .map(|connection| connection.session_id)
            .collect();

        match candidates.as_slice() {
            [] => Err(DomainError::ConnectionNotFound),
            [session_id] => Ok(*session_id),
            _ => Err(DomainError::AmbiguousTarget(target.to_owned())),
        }
    }

    /// 人工接管目标连接，AI 随即降为只读。
    ///
    /// # Errors
    ///
    /// 当指定会话不存在时返回错误。
    pub fn human_takeover(&mut self, session_id: SessionId) -> Result<(), DomainError> {
        let connection = self
            .connections
            .get_mut(&session_id)
            .ok_or(DomainError::ConnectionNotFound)?;
        connection.role = SessionRole::HumanControl;
        connection.updated_at = Utc::now();
        Ok(())
    }

    /// 释放人工接管，使会话回到 AI 按策略工作的状态。
    ///
    /// # Errors
    ///
    /// 当指定会话不存在时返回错误。
    pub fn release_human_takeover(&mut self, session_id: SessionId) -> Result<(), DomainError> {
        let connection = self
            .connections
            .get_mut(&session_id)
            .ok_or(DomainError::ConnectionNotFound)?;
        connection.role = SessionRole::AiControl;
        connection.updated_at = Utc::now();
        Ok(())
    }

    /// 更新连接状态。
    ///
    /// # Errors
    ///
    /// 当指定会话不存在时返回错误。
    pub fn set_state(
        &mut self,
        session_id: SessionId,
        state: ConnectionState,
    ) -> Result<(), DomainError> {
        let connection = self
            .connections
            .get_mut(&session_id)
            .ok_or(DomainError::ConnectionNotFound)?;
        connection.state = state;
        connection.updated_at = Utc::now();
        Ok(())
    }

    /// 将全部活动连接标记为传输重连中。
    pub fn mark_all_reconnecting(&mut self) {
        let now = Utc::now();
        for connection in self.connections.values_mut() {
            if connection.state != ConnectionState::Closed {
                connection.state = ConnectionState::Reconnecting;
                connection.updated_at = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use remoteops_domain::{
        AgentInstanceId, CapabilitySet, ConnectionState, PermissionMode, SessionRole,
    };

    use super::*;

    fn connection(session_id: SessionId) -> ConnectionDescriptor {
        ConnectionDescriptor {
            session_id,
            agent_instance_id: AgentInstanceId::new(),
            display_index: 0,
            alias: None,
            hostname: "test-host".to_owned(),
            mac_address: None,
            operating_system: "Windows".to_owned(),
            capabilities: CapabilitySet::default(),
            environment: remoteops_domain::EnvironmentProfile::empty(),
            credential_encryption_public_key: String::new(),
            credential_encryption_key_id: String::new(),
            state: ConnectionState::Online,
            role: SessionRole::AiReadOnly,
            permission_mode: PermissionMode::ReadOnly,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn assigns_stable_default_indices() {
        let mut registry = ConnectionRegistry::new();
        let first_id = SessionId::new();
        let second_id = SessionId::new();

        let first = registry.upsert(connection(first_id));
        let second = registry.upsert(connection(second_id));
        let updated = registry.upsert(connection(first_id));

        assert_eq!(first.display_name(), "连接 1");
        assert_eq!(second.display_name(), "连接 2");
        assert_eq!(updated.display_index, 1);
    }

    #[test]
    fn preserves_alias_and_index_when_session_changes_for_same_agent() {
        let mut registry = ConnectionRegistry::new();
        let old_session_id = SessionId::new();
        let new_session_id = SessionId::new();
        let mut first = connection(old_session_id);
        let agent_instance_id = first.agent_instance_id;
        first.alias = Some("客户 A".to_owned());
        registry.upsert(first);

        let mut replacement = connection(new_session_id);
        replacement.agent_instance_id = agent_instance_id;
        let replacement = registry.upsert(replacement);

        assert!(registry.get(old_session_id).is_none());
        assert_eq!(replacement.display_index, 1);
        assert_eq!(replacement.alias.as_deref(), Some("客户 A"));
    }

    #[test]
    fn marks_active_connections_as_reconnecting() {
        let mut registry = ConnectionRegistry::new();
        let session_id = SessionId::new();
        registry.upsert(connection(session_id));

        registry.mark_all_reconnecting();

        assert_eq!(
            registry.get(session_id).map(|connection| connection.state),
            Some(ConnectionState::Reconnecting)
        );
    }

    #[test]
    fn releases_human_takeover_back_to_ai_control() {
        let mut registry = ConnectionRegistry::new();
        let session_id = SessionId::new();
        registry.upsert(connection(session_id));

        registry.human_takeover(session_id).expect("人工接管应成功");
        registry
            .release_human_takeover(session_id)
            .expect("释放人工接管应成功");

        assert_eq!(
            registry.get(session_id).map(|connection| connection.role),
            Some(SessionRole::AiControl)
        );
    }

    #[test]
    fn resolves_alias_and_rejects_ambiguity() {
        let mut registry = ConnectionRegistry::new();
        let first_id = SessionId::new();
        let second_id = SessionId::new();
        registry.upsert(connection(first_id));
        registry.upsert(connection(second_id));
        registry
            .set_alias(first_id, "客户 A")
            .expect("应能设置别名");
        registry
            .set_alias(second_id, "客户 A")
            .expect("应能设置别名");

        assert!(matches!(
            registry.resolve("客户 A"),
            Err(DomainError::AmbiguousTarget(_))
        ));
        assert_eq!(
            registry.resolve("连接 2").expect("应解析默认编号"),
            second_id
        );
        assert_eq!(
            registry
                .resolve(&first_id.to_string())
                .expect("应解析会话标识"),
            first_id
        );
    }
}
