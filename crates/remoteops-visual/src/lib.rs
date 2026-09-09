//! `RemoteOps` AI 图形 Provider 抽象。

use async_trait::async_trait;
use remoteops_domain::{RequestId, SessionId, VisualActionResult, VisualObservation, VisualTarget};
use thiserror::Error;

#[cfg(windows)]
pub mod windows_provider;

/// Provider 运行错误。
#[derive(Debug, Error)]
pub enum VisualProviderError {
    /// 当前没有可用的交互式桌面。
    #[error("没有可用的交互式桌面")]
    NoInteractiveDesktop,
    /// Provider 尚未连接或已停止。
    #[error("图形 Provider 未连接")]
    Unavailable,
    /// Provider 返回了安全拒绝。
    #[error("图形操作被拒绝：{0}")]
    Rejected(String),
    /// Provider 协议或进程错误。
    #[error("图形 Provider 错误：{0}")]
    Protocol(String),
}

/// 交互式 Windows 图形 Provider 的最小接口。
#[async_trait]
pub trait VisualProvider: Send + Sync {
    /// 获取当前桌面状态。
    async fn observe(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError>;

    /// 等待 Provider 内部状态条件满足。
    async fn wait_for(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        condition: &str,
        timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError>;

    /// 调用 UIA 控件动作。
    async fn invoke(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        action: &str,
    ) -> Result<VisualActionResult, VisualProviderError>;

    /// 向 UIA 文本控件输入文字。
    async fn type_text(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        text: &str,
    ) -> Result<VisualActionResult, VisualProviderError>;

    /// 发送经过坐标校验的输入回退。
    async fn send_input(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        input: &str,
    ) -> Result<VisualActionResult, VisualProviderError>;

    /// 停止当前图形会话。
    async fn stop(&self, session_id: SessionId) -> Result<(), VisualProviderError>;
}

/// 默认 Provider，明确报告当前宿主没有交互式图形能力。
#[derive(Default)]
pub struct UnavailableVisualProvider;

#[async_trait]
impl VisualProvider for UnavailableVisualProvider {
    async fn observe(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _include_screenshot: bool,
        _include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn wait_for(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _condition: &str,
        _timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn invoke(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _action: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn type_text(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _text: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn send_input(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _input: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
}

/// 跨平台测试使用的内存 Provider。
#[derive(Default)]
pub struct MockVisualProvider;

#[async_trait]
impl VisualProvider for MockVisualProvider {
    async fn observe(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError> {
        Ok(VisualObservation {
            request_id,
            session_id,
            provider_instance_id: "mock".to_owned(),
            state: remoteops_domain::VisualSessionState::Ready,
            windows: Vec::new(),
            displays: Vec::new(),
            active_window_fingerprint: None,
            ui_tree: include_ui_tree.then(|| serde_json::json!({"mock": true})),
            screenshot_base64: include_screenshot.then(|| "mock".to_owned()),
            screenshot_width: include_screenshot.then_some(1),
            screenshot_height: include_screenshot.then_some(1),
            redacted: true,
        })
    }

    async fn wait_for(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        _condition: &str,
        _timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError> {
        self.observe(request_id, session_id, false, true).await
    }

    async fn invoke(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        _target: &VisualTarget,
        _action: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Ok(VisualActionResult {
            request_id,
            session_id,
            action_sent: true,
            effect_verified: true,
            observation: Some(self.observe(request_id, session_id, false, true).await?),
            error_code: None,
            message: "mock invoke completed".to_owned(),
        })
    }

    async fn type_text(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        _text: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        self.invoke(request_id, session_id, target, "type_text")
            .await
    }

    async fn send_input(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        _input: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        self.invoke(request_id, session_id, target, "send_input")
            .await
    }

    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_provider_returns_redacted_observation_and_verified_action() {
        let provider = MockVisualProvider;
        let session_id = SessionId::new();
        let observation = provider
            .observe(RequestId::new(), session_id, true, true)
            .await
            .expect("mock observation should succeed");
        assert!(observation.redacted);
        assert!(observation.screenshot_base64.is_some());
        assert!(observation.ui_tree.is_some());

        let target = VisualTarget::Control {
            window_fingerprint: "window".to_owned(),
            automation_id: Some("ok".to_owned()),
            name: Some("OK".to_owned()),
            control_type: Some("button".to_owned()),
            target_fingerprint: "control".to_owned(),
        };
        let result = provider
            .invoke(RequestId::new(), session_id, &target, "invoke")
            .await
            .expect("mock invoke should succeed");
        assert!(result.action_sent);
        assert!(result.effect_verified);
    }
}
