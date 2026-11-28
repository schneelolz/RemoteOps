//! 工具中心独立管理通道；不依赖目标会话，不自动重放管理操作。
use super::{
    ApplicationError, AtomicU64, BTreeMap, Ordering, RelayClient, TokioDuration, WireMessage,
    oneshot, timeout,
};
use remoteops_tool_center::{ToolCenterRequest, ToolCenterResponse};

#[derive(Default)]
pub(super) struct ToolCenterClientState {
    supported: AtomicU64,
    pending: std::sync::Arc<std::sync::Mutex<BTreeMap<String, oneshot::Sender<serde_json::Value>>>>,
}
impl ToolCenterClientState {
    pub(super) fn set_capability(&self, version: Option<u16>) {
        self.supported
            .store(u64::from(version.unwrap_or(0)), Ordering::Release);
    }
    pub(super) fn disconnect(&self) {
        self.supported.store(0, Ordering::Release);
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
    pub(super) fn handle(&self, message: &WireMessage) -> bool {
        match message {
            WireMessage::ToolCenterCapabilities { schema_version } => {
                self.supported
                    .store(u64::from(*schema_version), Ordering::Release);
                true
            }
            WireMessage::ToolCenterResponse {
                request_id,
                response,
            } => {
                if let Some(sender) = self
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(request_id)
                {
                    let _ = sender.send(response.clone());
                }
                true
            }
            _ => false,
        }
    }
}
struct PendingManagement {
    state: std::sync::Arc<std::sync::Mutex<BTreeMap<String, oneshot::Sender<serde_json::Value>>>>,
    request_id: String,
}
impl Drop for PendingManagement {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.request_id);
    }
}
impl RelayClient {
    /// 发送独立工具库管理请求，断线或超时后报告结果未知且不自动重放。
    ///
    /// # Errors
    /// Relay 缺少能力、权限拒绝、序列化或传输失败时返回错误。
    pub async fn tool_center_request(
        &self,
        request: ToolCenterRequest,
    ) -> Result<ToolCenterResponse, ApplicationError> {
        if self.inner.tool_center.supported.load(Ordering::Acquire) != 1 {
            return Err(ApplicationError::Transport(
                "tool_center_unsupported: 请升级配套 Relay/MCP".into(),
            ));
        }
        let request_id = request.request_id.clone();
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .inner
                .tool_center
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.retain(|_, sender| !sender.is_closed());
            if pending.contains_key(&request_id) {
                return Err(ApplicationError::Transport(
                    "duplicate_inflight_request: 相同请求正在处理中".into(),
                ));
            }
            pending.insert(request_id.clone(), sender);
        }
        let _registration = PendingManagement {
            state: self.inner.tool_center.pending.clone(),
            request_id: request_id.clone(),
        };
        let value = serde_json::to_value(request)
            .map_err(|_| ApplicationError::Transport("管理请求序列化失败".into()))?;
        if let Err(error) = self
            .send_wire(WireMessage::ToolCenterRequest {
                request_id: request_id.clone(),
                request: value,
            })
            .await
        {
            self.inner
                .tool_center
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&request_id);
            return Err(error);
        }
        let result = timeout(TokioDuration::from_mins(6), receiver).await;
        self.inner
            .tool_center
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&request_id);
        let value = result.ok().and_then(Result::ok).ok_or_else(|| {
            ApplicationError::Transport(
                "result_unknown: 管理请求可能已提交，请读取当前修订号核实；不得自动重放".into(),
            )
        })?;
        serde_json::from_value(value)
            .map_err(|_| ApplicationError::Transport("result_unknown: 管理响应无法解析".into()))
    }
}
