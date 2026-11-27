//! 同一画面流的控制指令。续租独立于输入积压，移动只与相邻的同代移动合并。
use remoteops_protocol::desktop::{Action, Input, LEASE};
use std::{collections::VecDeque, sync::Mutex, time::Instant};

pub(super) enum Command {
    Wire(Action),
    ReleaseOverload,
}

struct Pending {
    commands: VecDeque<Command>,
    heartbeat: Instant,
}

pub(super) struct Inbox(Mutex<Pending>);

impl Inbox {
    pub(super) fn new() -> Self {
        Self(Mutex::new(Pending {
            commands: VecDeque::new(),
            heartbeat: Instant::now(),
        }))
    }
    pub(super) fn push(&self, action: Action) {
        let mut pending = self.0.lock().unwrap();
        if matches!(action, Action::Heartbeat) {
            pending.heartbeat = Instant::now();
            return;
        }
        let queue = &mut pending.commands;
        if let Action::Input {
            epoch,
            event: Input::Move { .. },
        } = &action
            && matches!(queue.back(), Some(Command::Wire(Action::Input {epoch: prior, event:Input::Move{..}})) if prior == epoch)
        {
            queue.pop_back();
        }
        if matches!(action, Action::Control { enabled: false, .. }) {
            queue.clear();
        }
        if queue.len() >= 64 {
            queue.clear();
            queue.push_back(Command::ReleaseOverload);
        } else {
            queue.push_back(Command::Wire(action));
        }
    }
    pub(super) fn next(&self) -> Option<Command> {
        self.0.lock().unwrap().commands.pop_front()
    }
    pub(super) fn heartbeat_time(&self) -> Instant {
        self.0.lock().unwrap().heartbeat
    }
    pub(super) fn live(&self) -> bool {
        self.0.lock().unwrap().heartbeat.elapsed() < LEASE
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.lock().unwrap().commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn movement(x: u16) -> Action {
        Action::Input {
            epoch: 3,
            event: Input::Move { x, y: 1 },
        }
    }
    #[test]
    fn mouse_bursts_keep_last_position_on_each_side_of_click() {
        let inbox = Inbox::new();
        for x in 0..4096 {
            inbox.push(movement(x));
        }
        let click = Action::Input {
            epoch: 3,
            event: Input::Button {
                button: 0,
                down: true,
            },
        };
        inbox.push(click.clone());
        for x in 5000..6000 {
            inbox.push(movement(x));
        }
        for expected in [movement(4095), click, movement(5999)] {
            assert!(matches!(inbox.next(),Some(Command::Wire(actual)) if actual==expected));
        }
        assert!(inbox.is_empty());
    }
    #[test]
    fn saturated_input_releases_and_can_receive_heartbeats_and_new_grants() {
        let inbox = Inbox::new();
        for _ in 0..65 {
            inbox.push(Action::Input {
                epoch: 3,
                event: Input::Wheel { delta: 120 },
            });
        }
        inbox.push(Action::Heartbeat);
        assert!(inbox.live());
        assert!(matches!(inbox.next(), Some(Command::ReleaseOverload)));
        assert!(inbox.is_empty());
        inbox.push(Action::Control {
            enabled: true,
            epoch: 4,
        });
        assert!(matches!(
            inbox.next(),
            Some(Command::Wire(Action::Control {
                enabled: true,
                epoch: 4
            }))
        ));
    }
}
