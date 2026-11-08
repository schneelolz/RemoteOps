use crate::ResourceTool;
use serde::{Deserialize, Serialize};

/// 中继登记的精确测试证据，按登记顺序排列。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformEvidence {
    /// 实际操作系统。
    pub os: String,
    /// 实际架构。
    pub arch: String,
    /// 实测 Windows 内部版本。
    pub windows_build: Option<u32>,
    /// 是否使用交互桌面。
    pub desktop: bool,
    /// passed、failed 或 pending。
    pub status: String,
}

/// 当前平台的验证结论，与运行授权分别判断。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformState {
    /// 当前组合验证通过。
    Verified,
    /// 无覆盖当前组合的有效证据。
    Unverified,
    /// 当前组合最近一次测试失败。
    Failed,
    /// 当前组合明确声明不支持。
    Unsupported,
}

/// 可供模型和界面直接解释的平台判断。
#[derive(Clone, Debug, Serialize)]
pub struct PlatformAssessment {
    /// 独立于运行策略的验证状态。
    pub status: PlatformState,
    /// 固定、无敏感数据的原因。
    pub reason: &'static str,
    /// 当前平台能否正常启动；仍需通过权限与包校验。
    pub normal_start_allowed: bool,
    /// 当前平台可否请求受控测试。
    pub validation_available: bool,
    /// 实际读取的 Windows build；未知时为空。
    pub windows_build: Option<u32>,
}

impl ResourceTool {
    pub(crate) fn check_start_platform(&self, validation_mode: bool) -> crate::Result<()> {
        let platform = self.platform_assessment();
        if platform.normal_start_allowed || validation_mode && platform.validation_available {
            return Ok(());
        }
        Err(crate::ResourceError::before(
            platform.reason,
            match platform.status {
                PlatformState::Unsupported => "当前平台已声明不支持，不能启动",
                PlatformState::Failed => "当前平台测试失败；正常启动不允许",
                _ => "当前平台尚未验证；可显式请求受控兼容性测试",
            },
        ))
    }

    /// 根据实际平台读取验证资格。
    #[must_use]
    pub fn platform_assessment(&self) -> PlatformAssessment {
        self.assess_platform(
            std::env::consts::OS,
            std::env::consts::ARCH,
            crate::manifest::current_windows_build(),
        )
    }

    /// 根据给定平台匹配精确证据，不将两个离散 build 扩展成区间。
    #[must_use]
    pub fn assess_platform(&self, os: &str, arch: &str, build: Option<u32>) -> PlatformAssessment {
        let matches_build = |min: Option<u32>, max: Option<u32>| {
            os != "windows"
                || matches!((build,min,max), (Some(b),Some(lo),Some(hi)) if b >= lo && b <= hi)
        };
        let status = if self.platforms.iter().any(|p| {
            p.os == os
                && p.arch == arch
                && p.status == "unsupported"
                && ((p.min_windows_build.is_none() && p.max_windows_build.is_none())
                    || matches_build(p.min_windows_build, p.max_windows_build))
        }) {
            PlatformState::Unsupported
        } else if os == "windows" && build.is_none() {
            PlatformState::Unverified
        } else if let Some(test) = self.platform_tests.iter().rev().find(|t| {
            t.os == os
                && t.arch == arch
                && (os != "windows" || t.windows_build == build)
                && (!self.requires_desktop || t.desktop)
        }) {
            match test.status.as_str() {
                "passed" if !self.requires_desktop || test.desktop => PlatformState::Verified,
                "failed" => PlatformState::Failed,
                _ => PlatformState::Unverified,
            }
        } else if self
            .platform_tests
            .iter()
            .any(|t| t.os == os && t.arch == arch)
        {
            PlatformState::Unverified
        } else if self.platforms.iter().any(|p| {
            p.os == os
                && p.arch == arch
                && p.status == "passed"
                && matches_build(p.min_windows_build, p.max_windows_build)
        }) {
            PlatformState::Verified
        } else {
            PlatformState::Unverified
        };
        PlatformAssessment {
            status,
            reason: match status {
                PlatformState::Verified => "platform_verified",
                PlatformState::Unverified => "platform_unverified",
                PlatformState::Failed => "platform_test_failed",
                PlatformState::Unsupported => "platform_unsupported",
            },
            normal_start_allowed: status == PlatformState::Verified,
            validation_available: status != PlatformState::Unsupported
                && self.platforms.iter().any(|p| p.os == os && p.arch == arch),
            windows_build: build,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tool() -> ResourceTool {
        crate::validate_manifest(include_bytes!(
            "../../../examples/resource-site/remoteops/v1/index.json"
        ))
        .unwrap()
        .tools
        .remove(0)
    }
    fn evidence(build: u32, status: &str) -> PlatformEvidence {
        PlatformEvidence {
            os: "windows".into(),
            arch: "x86_64".into(),
            windows_build: Some(build),
            desktop: true,
            status: status.into(),
        }
    }
    #[test]
    fn exact_build_evidence_keeps_independent_results_and_never_fills_gaps() {
        let mut tool = tool();
        tool.platform_tests = vec![evidence(26100, "passed"), evidence(26200, "failed")];
        assert_eq!(
            tool.assess_platform("windows", "x86_64", Some(26100))
                .status,
            PlatformState::Verified
        );
        assert_eq!(
            tool.assess_platform("windows", "x86_64", Some(26150))
                .status,
            PlatformState::Unverified
        );
        assert_eq!(
            tool.assess_platform("windows", "x86_64", Some(26200))
                .status,
            PlatformState::Failed
        );
        tool.platform_tests.push(evidence(26200, "passed"));
        assert_eq!(
            tool.assess_platform("windows", "x86_64", Some(26200))
                .status,
            PlatformState::Verified
        );
        assert_eq!(
            tool.assess_platform("windows", "x86_64", None).status,
            PlatformState::Unverified
        );
        assert!(
            !tool
                .assess_platform("windows", "aarch64", Some(26200))
                .normal_start_allowed
        );
    }
    #[test]
    fn unsupported_and_desktop_requirements_are_not_bypassed() {
        let mut tool = tool();
        tool.requires_desktop = true;
        let mut headless = evidence(26200, "passed");
        headless.desktop = false;
        tool.platform_tests = vec![headless];
        assert_eq!(
            tool.assess_platform("windows", "x86_64", Some(26200))
                .status,
            PlatformState::Unverified
        );
        tool.platforms.push(crate::Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            os_versions: "explicit".into(),
            status: "unsupported".into(),
            min_windows_build: Some(26200),
            max_windows_build: Some(26200),
        });
        let state = tool.assess_platform("windows", "x86_64", Some(26200));
        assert_eq!(state.status, PlatformState::Unsupported);
        assert!(!state.validation_available);
    }
}
