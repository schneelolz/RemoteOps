//! Host-independent product defaults and immutable endpoint policy.
use anyhow::{Result, bail};
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointPolicy {
    Default { relay: String, server_name: String },
    Fixed { relay: String, server_name: String },
}
impl EndpointPolicy {
    #[must_use]
    pub fn endpoint(&self) -> (&str, &str) {
        match self {
            Self::Default { relay, server_name } | Self::Fixed { relay, server_name } => {
                (relay, server_name)
            }
        }
    }
    #[must_use]
    pub const fn is_fixed(&self) -> bool {
        matches!(self, Self::Fixed { .. })
    }
    /// # Errors
    /// Rejects every conflicting endpoint, regardless of its input source.
    pub fn check(&self, relay: &str, server_name: &str) -> Result<()> {
        if self.is_fixed() && self.endpoint() != (relay.trim(), server_name.trim()) {
            bail!("Product endpoint policy rejects this Relay or TLS server name");
        }
        Ok(())
    }
}

/// 产品编译时确定的能力上限；配置、权限授权与宿主参数只能进一步收紧。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AgentCapabilityPolicy {
    // 禁止所有内建图形操作及真实 Provider 生命周期。
    visual_disabled: bool,
    // 仅允许由公共策略重新计算为只读的操作。
    read_only: bool,
    // 宿主能力位图，None 保留现有产品的默认能力。
    allowed: Option<u64>,
}
impl AgentCapabilityPolicy {
    /// 设置能力上限；重复设置只能收紧。
    /// # Errors
    /// 未知能力名称返回配置错误。
    pub fn with_allowed_capabilities(mut self, names: &[String]) -> Result<Self> {
        let mut bits = 0;
        for name in names {
            bits |= capability_bit(name).ok_or_else(|| anyhow::anyhow!("未知宿主能力: {name}"))?;
        }
        self.allowed = Some(self.allowed.map_or(bits, |current| current & bits));
        Ok(self)
    }

    /// 判断宿主是否开放该能力。
    #[must_use]
    pub fn allows(self, name: &str) -> bool {
        self.allowed
            .is_none_or(|allowed| capability_bit(name).is_some_and(|bit| allowed & bit != 0))
    }
    /// 创建强制无图形产品上限。
    #[must_use]
    pub const fn headless() -> Self {
        Self {
            visual_disabled: true,
            read_only: false,
            allowed: None,
        }
    }
    /// 进一步限制为只读产品。
    #[must_use]
    pub const fn with_read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
    /// 关闭桌面能力并保留其他已有约束。
    #[must_use]
    pub const fn without_visual(mut self) -> Self {
        self.visual_disabled = true;
        self
    }
    /// 产品是否允许图形操作。
    #[must_use]
    pub fn visual_allowed(self) -> bool {
        !self.visual_disabled && self.allows("visual")
    }
    /// 产品是否仅允许只读操作。
    #[must_use]
    pub const fn read_only(self) -> bool {
        self.read_only
    }
}

fn capability_bit(name: &str) -> Option<u64> {
    [
        "cmd",
        "windows_power_shell",
        "power_shell",
        "ssh",
        "serial",
        "file_transfer",
        "port_probe",
        "tcp_exchange",
        "tcp_forward",
        "system_operations",
        "visual",
        "resource_tools",
        "host_business",
    ]
    .iter()
    .position(|candidate| *candidate == name)
    .map(|index| 1_u64 << index)
}

#[derive(Clone, Debug)]
pub struct ProductProfile {
    pub id: String,
    pub name: String,
    pub version: String,
    pub directory_name: String,
    pub config_file_name: String,
    pub state_file_name: String,
    pub env_prefix: String,
    pub endpoint: EndpointPolicy,
    pub default_full_control: bool,
    /// Preserve existing `RemoteOps` discovery for the default product only.
    pub legacy_remoteops_paths: bool,
}
impl Default for ProductProfile {
    fn default() -> Self {
        Self {
            id: "remoteops-agent-gui".into(),
            name: "RemoteOps".into(),
            version: String::new(),
            directory_name: "RemoteOps".into(),
            config_file_name: "agent-config.json".into(),
            state_file_name: "agent-state.json".into(),
            env_prefix: "REMOTEOPS".into(),
            endpoint: EndpointPolicy::Default {
                relay: String::new(),
                server_name: String::new(),
            },
            default_full_control: false,
            legacy_remoteops_paths: true,
        }
    }
}
impl ProductProfile {
    /// # Errors
    /// Names must be safe individual path components; environment prefix must be portable.
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.id,
            &self.directory_name,
            &self.config_file_name,
            &self.state_file_name,
        ] {
            if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\', ':'])
            {
                bail!("Invalid product path component");
            }
        }
        if self.env_prefix.is_empty()
            || !self
                .env_prefix
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("Invalid product environment prefix");
        }
        Ok(())
    }
    #[must_use]
    pub fn paths(&self) -> ProductPaths {
        let executable = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| ".".into());
        let user = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| env::var_os("XDG_DATA_HOME").map(PathBuf::from))
            .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
            .unwrap_or_else(|| ".".into())
            .join(&self.directory_name);
        self.paths_at(&executable, &user)
    }
    #[must_use]
    pub fn paths_at(&self, executable: &Path, user_directory: &Path) -> ProductPaths {
        ProductPaths {
            portable_config: executable.join(&self.config_file_name),
            user_config: user_directory.join(&self.config_file_name),
            state_file: user_directory.join(&self.state_file_name),
            transfer_root: user_directory.join("transfer"),
            gui_preferences: user_directory.join("gui.ron"),
            diagnostics: user_directory.join("logs"),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ProductPaths {
    pub portable_config: PathBuf,
    pub user_config: PathBuf,
    pub state_file: PathBuf,
    pub transfer_root: PathBuf,
    pub gui_preferences: PathBuf,
    pub diagnostics: PathBuf,
}
impl ProductPaths {
    #[must_use]
    pub fn active_config(&self) -> PathBuf {
        if self.portable_config.is_file() || !self.user_config.is_file() {
            self.portable_config.clone()
        } else {
            self.user_config.clone()
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_allowlist_can_only_shrink() {
        let policy = AgentCapabilityPolicy::headless()
            .with_read_only()
            .with_allowed_capabilities(&["cmd".into(), "host_business".into()])
            .unwrap();
        assert!(policy.allows("cmd"));
        assert!(!policy.visual_allowed());
        assert!(!policy.allows("file_transfer"));
        let smaller = policy
            .with_allowed_capabilities(&["cmd".into(), "ssh".into()])
            .unwrap();
        assert!(smaller.allows("cmd"));
        assert!(!smaller.allows("ssh"));
        assert!(smaller.without_visual().read_only());
        assert!(policy.with_allowed_capabilities(&["typo".into()]).is_err());
    }
    #[test]
    fn defaults_and_paths() {
        let a = ProductProfile::default();
        assert!(!a.default_full_control);
        let mut b = a.clone();
        b.directory_name = "ExampleAssist".into();
        b.config_file_name = "example.json".into();
        assert_ne!(a.paths().state_file, b.paths().state_file);
        assert_ne!(a.paths().portable_config, b.paths().portable_config);
        b.config_file_name = "../bad".into();
        assert!(b.validate().is_err());
    }
}
