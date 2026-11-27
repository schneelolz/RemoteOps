#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RtcBackend {
    MiniRtc,
    CrossDesk,
}

impl RtcBackend {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::MiniRtc => "MiniRTC",
            Self::CrossDesk => "CrossDesk",
        }
    }
}

pub(super) struct RoutePlan {
    pub(super) waiting_for_lan: bool,
    pub(super) upgrade: Option<RtcBackend>,
}

impl RoutePlan {
    pub(super) fn new(backend: &str) -> Self {
        use RtcBackend::*;
        let (waiting_for_lan, upgrade) = match backend {
            "auto" => (true, Some(MiniRtc)),
            "minirtc" => (false, Some(MiniRtc)),
            "crossdesk" => (false, Some(CrossDesk)),
            "tcp" => (false, None),
            _ => (true, None),
        };
        Self { waiting_for_lan, upgrade }
    }

    // 仅在首条 Geometry 上取双方共同支持的路线；返回缺少的后端供界面说明。
    pub(super) fn negotiate(&mut self, lan: bool, mini_rtc: bool, crossdesk: bool) -> Option<RtcBackend> {
        self.waiting_for_lan &= lan;
        match self.upgrade {
            Some(RtcBackend::MiniRtc) if !mini_rtc => self.upgrade.take(),
            Some(RtcBackend::CrossDesk) if !crossdesk => self.upgrade.take(),
            _ => None,
        }
    }

    // 一个桌面流最多申请一次媒体升级。LAN 已连接时，继续保留断线后的候选。
    pub(super) fn take_upgrade(&mut self) -> Option<RtcBackend> {
        if self.waiting_for_lan { None } else { self.upgrade.take() }
    }
}

#[cfg(test)]
mod tests {
    use super::{RoutePlan, RtcBackend::*};

    #[test]
    fn auto_waits_for_lan_then_tries_minirtc_once() {
        let mut auto = RoutePlan::new("auto");
        assert_eq!(auto.negotiate(true, true, true), None);
        assert!(auto.waiting_for_lan);
        assert_eq!(auto.take_upgrade(), None);
        assert_eq!(auto.take_upgrade(), None);
        auto.waiting_for_lan = false;
        assert_eq!(auto.take_upgrade(), Some(MiniRtc));
        assert_eq!(auto.take_upgrade(), None);
    }

    #[test]
    fn no_lan_starts_minirtc_immediately_and_old_endpoints_remain_on_tcp() {
        let mut wan = RoutePlan::new("auto");
        wan.negotiate(false, true, true);
        assert_eq!(wan.take_upgrade(), Some(MiniRtc));
        for lan in [true, false] {
            let mut legacy = RoutePlan::new("auto");
            assert_eq!(legacy.negotiate(lan, false, true), Some(MiniRtc));
            assert_eq!(legacy.waiting_for_lan, lan);
            legacy.waiting_for_lan = false;
            assert_eq!(legacy.take_upgrade(), None);
        }
    }

    #[test]
    fn manual_routes_skip_lan_and_never_try_a_different_rtc_backend() {
        for (name, expected) in [("tcp", None), ("minirtc", Some(MiniRtc)), ("crossdesk", Some(CrossDesk))] {
            let mut plan = RoutePlan::new(name);
            assert!(!plan.waiting_for_lan);
            assert_eq!(plan.take_upgrade(), expected);
            assert_eq!(plan.take_upgrade(), None);
        }
    }
}
