//! Bounded activity view driven exclusively by actual task events.
use crate::egui;
use remoteops_agent::{AgentEvent, AgentOperationState};
use remoteops_domain::RequestId;
use remoteops_i18n::Language;
use std::collections::{BTreeSet, VecDeque};
#[derive(Clone, Debug)]
pub struct Summary {
    pub text: String,
    pub repetitions: usize,
    pub terminal: Option<AgentOperationState>,
}
#[derive(Default)]
pub struct ActivityModel {
    connected: bool,
    ran: bool,
    running: BTreeSet<RequestId>,
    completed: BTreeSet<RequestId>,
    summaries: VecDeque<Summary>,
}
impl ActivityModel {
    pub fn disconnect(&mut self) {
        self.connected = false;
        self.ran = false;
        self.completed.extend(self.running.iter().copied());
        self.running.clear();
        self.summaries.clear();
    }
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.running.len()
    }
    #[must_use]
    pub fn summaries(&self) -> &VecDeque<Summary> {
        &self.summaries
    }
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ControllerCountChanged { active_connections } => {
                if *active_connections == 0 {
                    self.disconnect();
                } else {
                    self.connected = true;
                }
            }
            AgentEvent::Connecting
            | AgentEvent::Reconnecting { .. }
            | AgentEvent::Stopped
            | AgentEvent::Failed { .. } => self.disconnect(),
            AgentEvent::OperationStateChanged { request_id, state } if self.connected => {
                if *state == AgentOperationState::Running {
                    if !self.completed.contains(request_id) {
                        self.running.insert(*request_id);
                        self.ran = true;
                    }
                } else {
                    self.running.remove(request_id);
                    if self.completed.insert(*request_id) {
                        self.push("", Some(*state));
                    }
                    while self.completed.len() > 1024 {
                        self.completed.pop_first();
                    }
                }
            }
            AgentEvent::OperationLog(log) if self.connected => self.push(&log.message, None),
            _ => {}
        }
    }
    fn push(&mut self, text: &str, terminal: Option<AgentOperationState>) {
        let text: String = text.chars().take(180).collect();
        if let Some(last) = self.summaries.back_mut()
            && last.text == text
            && last.terminal == terminal
        {
            last.repetitions = last.repetitions.saturating_add(1);
            return;
        }
        if self.summaries.len() == 64 {
            if terminal.is_none() && self.summaries.iter().all(|s| s.terminal.is_some()) {
                return;
            }
            let i = self
                .summaries
                .iter()
                .position(|s| s.terminal.is_none())
                .unwrap_or(0);
            self.summaries.remove(i);
        }
        self.summaries.push_back(Summary {
            text,
            terminal,
            repetitions: 1,
        });
    }
    pub fn render(&self, ui: &mut egui::Ui, language: Language) {
        let zh = language == Language::ZhCn;
        let active = !self.running.is_empty();
        let waiting = !active && self.connected && self.ran;
        let time = ui.input(|i| i.time);
        let dots = if time.fract() < 0.5 { "." } else { "…" };
        let title = if active {
            if zh { "执行中" } else { "Executing" }
        } else if waiting {
            if zh {
                "等待下一步"
            } else {
                "Waiting for next step"
            }
        } else if zh {
            "活动"
        } else {
            "Activity"
        };
        ui.label(format!("{title}{}", if waiting { dots } else { "" }));
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 2.0), egui::Sense::hover());
        if active {
            #[allow(clippy::cast_possible_truncation)]
            let phase = (time.fract()) as f32;
            let x = rect.left() + phase * rect.width();
            ui.painter().hline(
                x..=(x + 40.0).min(rect.right()),
                rect.center().y,
                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
            );
        }
        egui::ScrollArea::vertical()
            .id_salt("product-activity")
            .max_height(80.0)
            .min_scrolled_height(80.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for row in &self.summaries {
                    let text = row
                        .terminal
                        .map_or(row.text.as_str(), |state| terminal_text(state, zh));
                    let text = if row.repetitions > 1 {
                        format!("{text} ×{}", row.repetitions)
                    } else {
                        text.to_owned()
                    };
                    ui.add_sized(
                        egui::vec2(ui.available_width(), 20.0),
                        egui::Label::new(text).truncate(),
                    );
                }
            });
        if (active || waiting)
            && ui.is_rect_visible(ui.min_rect())
            && !ui.input(|i| i.viewport().minimized.unwrap_or(false))
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(if active {
                    80
                } else {
                    500
                }));
        }
    }
}
fn terminal_text(state: AgentOperationState, zh: bool) -> &'static str {
    match (state, zh) {
        (AgentOperationState::Succeeded, true) => "执行成功",
        (AgentOperationState::Succeeded, false) => "Succeeded",
        (AgentOperationState::Failed, true) => "执行失败",
        (AgentOperationState::Failed, false) => "Failed",
        (AgentOperationState::Cancelled, true) => "已取消",
        (AgentOperationState::Cancelled, false) => "Cancelled",
        (AgentOperationState::Interrupted, true) => "已中断",
        (AgentOperationState::Interrupted, false) => "Interrupted",
        (AgentOperationState::Running, true) => "执行中",
        (AgentOperationState::Running, false) => "Running",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_terminal_and_disconnect() {
        let mut model = ActivityModel::default();
        model.observe(&AgentEvent::ControllerCountChanged {
            active_connections: 1,
        });
        let first = RequestId::new();
        let second = RequestId::new();
        for id in [first, second] {
            model.observe(&AgentEvent::OperationStateChanged {
                request_id: id,
                state: AgentOperationState::Running,
            });
        }
        model.observe(&AgentEvent::OperationStateChanged {
            request_id: first,
            state: AgentOperationState::Cancelled,
        });
        assert_eq!(model.running_count(), 1);
        model.observe(&AgentEvent::OperationStateChanged {
            request_id: first,
            state: AgentOperationState::Running,
        });
        assert_eq!(model.running_count(), 1);
        model.disconnect();
        model.observe(&AgentEvent::OperationStateChanged {
            request_id: second,
            state: AgentOperationState::Running,
        });
        assert_eq!(model.running_count(), 0);
    }
    #[test]
    fn bounded_unicode_and_terminal_priority() {
        let mut model = ActivityModel::default();
        model.push("", Some(AgentOperationState::Succeeded));
        for i in 0..100 {
            model.push(&format!("{i}{}", "界".repeat(300)), None);
        }
        model.push("duplicate", None);
        model.push("duplicate", None);
        assert_eq!(model.summaries.len(), 64);
        assert!(model.summaries.iter().any(|s| s.terminal.is_some()));
        assert!(
            model
                .summaries
                .iter()
                .all(|s| s.text.chars().count() <= 180)
        );
        assert_eq!(model.summaries.back().unwrap().repetitions, 2);
    }
}
