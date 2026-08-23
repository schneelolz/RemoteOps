//! GUI 时间线中的远程请求聚合模型。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use remoteops_domain::{
    ApprovalId, EventPayload, EventSource, RemoteEvent, RemoteOperation, RequestId,
};

/// 同一远程请求中的一段终端输出。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputSegment {
    /// 是否来自标准错误流。
    pub stderr: bool,
    /// 按事件到达顺序累积的文本。
    pub text: String,
}

/// 远程请求的最终状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestTerminal {
    /// 请求正常完成。
    Completed {
        /// 进程退出码。
        exit_code: Option<i32>,
        /// Agent 返回的结果摘要。
        summary: String,
    },
    /// 请求执行失败。
    Failed {
        /// 稳定错误代码。
        code: String,
        /// 面向用户的错误信息。
        message: String,
    },
    /// 请求已被用户取消。
    Cancelled,
}

/// 按 `request_id` 聚合后的远程任务。
#[derive(Clone, Debug)]
pub struct RequestTimelineItem {
    /// 原始远程请求标识。
    pub request_id: RequestId,
    /// 请求来源。
    pub source: EventSource,
    /// 首条事件时间。
    pub occurred_at: DateTime<Utc>,
    /// 请求执行的操作。
    pub operation: Option<RemoteOperation>,
    /// 是否已收到开始事件。
    pub started: bool,
    /// 合并后的输出片段。
    pub output: Vec<OutputSegment>,
    /// 可选审批信息。
    pub approval: Option<(ApprovalId, String)>,
    /// 可选最终状态。
    pub terminal: Option<RequestTerminal>,
}

/// GUI 时间线中的一项内容。
#[derive(Clone, Debug)]
pub enum TimelineItem {
    /// 一个按请求聚合的远程任务。
    Request(RequestTimelineItem),
    /// 不属于远程请求的普通会话事件。
    Event(RemoteEvent),
}

/// 把事件流按请求聚合，同时保留不同请求首次出现的先后顺序。
pub fn aggregate_events(events: impl IntoIterator<Item = RemoteEvent>) -> Vec<TimelineItem> {
    let mut items = Vec::new();
    let mut request_indexes = BTreeMap::new();

    for event in events {
        let Some(request_id) = event.request_id else {
            items.push(TimelineItem::Event(event));
            continue;
        };
        let request_key = (event.session_id, request_id);

        let index = if let Some(index) = request_indexes.get(&request_key).copied() {
            index
        } else {
            let index = items.len();
            request_indexes.insert(request_key, index);
            items.push(TimelineItem::Request(RequestTimelineItem {
                request_id,
                source: event.source,
                occurred_at: event.occurred_at,
                operation: None,
                started: false,
                output: Vec::new(),
                approval: None,
                terminal: None,
            }));
            index
        };

        let TimelineItem::Request(item) = &mut items[index] else {
            unreachable!("请求索引必须指向请求时间线项");
        };
        match event.payload {
            EventPayload::OperationRequested { operation } => item.operation = Some(operation),
            EventPayload::OperationStarted => item.started = true,
            EventPayload::OutputChunk { stderr, text } => {
                append_output(&mut item.output, stderr, text);
            }
            EventPayload::ApprovalRequired {
                approval_id,
                reason,
            } => item.approval = Some((approval_id, reason)),
            EventPayload::OperationCompleted { exit_code, summary } => {
                item.terminal = Some(RequestTerminal::Completed { exit_code, summary });
            }
            EventPayload::OperationFailed { code, message } => {
                item.terminal = Some(RequestTerminal::Failed { code, message });
            }
            EventPayload::OperationCancelled => item.terminal = Some(RequestTerminal::Cancelled),
            EventPayload::SessionOpened
            | EventPayload::SessionDisconnected
            | EventPayload::SessionResumed
            | EventPayload::SessionClosed => {}
        }
    }

    items
}

fn append_output(output: &mut Vec<OutputSegment>, stderr: bool, text: String) {
    if let Some(last) = output.last_mut()
        && last.stderr == stderr
    {
        last.text.push_str(&text);
        return;
    }
    output.push(OutputSegment { stderr, text });
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use remoteops_domain::{ApprovalState, RequestId, SessionId, ShellKind};

    use super::*;

    fn event(
        sequence: u64,
        session_id: SessionId,
        request_id: RequestId,
        payload: EventPayload,
    ) -> RemoteEvent {
        RemoteEvent {
            sequence,
            session_id,
            request_id: Some(request_id),
            source: EventSource::Human,
            approval: ApprovalState::NotRequired,
            payload,
            occurred_at: Utc::now(),
        }
    }

    #[test]
    fn aggregates_streamed_output_and_completion_into_one_request() {
        let session_id = SessionId::new();
        let request_id = RequestId::new();
        let items = aggregate_events([
            event(
                1,
                session_id,
                request_id,
                EventPayload::OperationRequested {
                    operation: RemoteOperation::RunCommand {
                        shell: ShellKind::WindowsPowerShell,
                        command: "ipconfig".to_owned(),
                        readonly: true,
                    },
                },
            ),
            event(
                2,
                session_id,
                request_id,
                EventPayload::OutputChunk {
                    stderr: false,
                    text: "Windows IP Configuration\r\n".to_owned(),
                },
            ),
            event(
                3,
                session_id,
                request_id,
                EventPayload::OutputChunk {
                    stderr: false,
                    text: "IPv4 Address: 192.0.2.117\r\n".to_owned(),
                },
            ),
            event(
                4,
                session_id,
                request_id,
                EventPayload::OperationCompleted {
                    exit_code: Some(0),
                    summary: "Windows IP Configuration\r\nIPv4 Address: 192.0.2.117\r\n".to_owned(),
                },
            ),
        ]);

        assert_eq!(items.len(), 1);
        let TimelineItem::Request(item) = &items[0] else {
            panic!("应聚合为请求时间线项");
        };
        assert_eq!(item.request_id, request_id);
        assert_eq!(item.output.len(), 1);
        assert_eq!(
            item.output[0].text,
            "Windows IP Configuration\r\nIPv4 Address: 192.0.2.117\r\n"
        );
        assert!(matches!(
            item.terminal,
            Some(RequestTerminal::Completed {
                exit_code: Some(0),
                ..
            })
        ));
    }

    #[test]
    fn preserves_stdout_and_stderr_arrival_order() {
        let session_id = SessionId::new();
        let request_id = RequestId::new();
        let items = aggregate_events([
            event(
                1,
                session_id,
                request_id,
                EventPayload::OutputChunk {
                    stderr: false,
                    text: "stdout-1\n".to_owned(),
                },
            ),
            event(
                2,
                session_id,
                request_id,
                EventPayload::OutputChunk {
                    stderr: true,
                    text: "stderr\n".to_owned(),
                },
            ),
            event(
                3,
                session_id,
                request_id,
                EventPayload::OutputChunk {
                    stderr: false,
                    text: "stdout-2\n".to_owned(),
                },
            ),
        ]);

        let TimelineItem::Request(item) = &items[0] else {
            panic!("应聚合为请求时间线项");
        };
        assert_eq!(item.output.len(), 3);
        assert_eq!(item.output[0].text, "stdout-1\n");
        assert!(item.output[1].stderr);
        assert_eq!(item.output[2].text, "stdout-2\n");
    }
}
