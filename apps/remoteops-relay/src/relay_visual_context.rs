use chrono::{DateTime, Utc};
use remoteops_domain::TargetContext;
use remoteops_protocol::WireMessage;
use serde::Deserialize;

// 只缓存图形可用性，配对、审批和其他产品能力继续由各自的入口校验。
#[derive(Clone, Default)]
pub(super) struct VisualContext {
    issued: u64,
    applied: u64,
    observed_at: Option<DateTime<Utc>>,
    decision: Option<Result<(), String>>,
}

impl VisualContext {
    pub(super) fn begin(&mut self, hello: Option<&TargetContext>) -> Option<u64> {
        if !hello.is_some_and(TargetContext::visual_status_refresh_allowed) {
            return None;
        }
        self.issued = self.issued.checked_add(1)?;
        Some(self.issued)
    }

    pub(super) fn disabled_reason<'a>(
        &'a self,
        hello: Option<&'a TargetContext>,
    ) -> Option<&'a str> {
        let initial = hello?;
        match &self.decision {
            Some(result) if initial.visual_status_refresh_allowed() => {
                result.as_ref().err().map(String::as_str)
            }
            _ => initial.visual_disabled_reason(),
        }
    }

    pub(super) fn finish(
        &mut self,
        sequence: u64,
        hello: Option<&TargetContext>,
        message: &WireMessage,
    ) {
        // 只比较已经返回的查询，另一控制端的并发轮询不会遮住刚收到的锁屏回执。
        if sequence <= self.applied {
            return;
        }
        self.applied = sequence;
        self.decision = Some(self.check_response(hello, message));
    }

    fn check_response(
        &mut self,
        hello: Option<&TargetContext>,
        message: &WireMessage,
    ) -> Result<(), String> {
        const UNKNOWN: &str = "host_context_unknown";
        let initial = hello.ok_or(UNKNOWN)?;
        let WireMessage::RemoteResponse(reply) = message else {
            return Err(UNKNOWN.into());
        };
        if reply.exit_code != Some(0) || reply.error_code.is_some() {
            return Err(UNKNOWN.into());
        }
        let report = TargetContext::deserialize(reply.details.as_ref().ok_or(UNKNOWN)?)
            .map_err(|_| UNKNOWN)?;
        if report.validate().is_err()
            || report.product_id != initial.product_id
            || report.version != initial.version
            || report.run_mode != initial.run_mode
            || report.facts.process_session_id != initial.facts.process_session_id
            || report.facts.observed_at < self.observed_at.unwrap_or(initial.facts.observed_at)
        {
            return Err(UNKNOWN.into());
        }
        self.observed_at = Some(report.facts.observed_at);
        if !report.visual_allowed() || !report.visual_probe_allowed() {
            return Err(report
                .visual_disabled_reason()
                .unwrap_or("visual_unavailable")
                .to_owned());
        }
        Ok(())
    }
}
