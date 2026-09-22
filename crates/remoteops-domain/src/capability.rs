use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Agent 可公开的远程能力。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Windows CMD 或平台默认命令解释器。
    Cmd,
    /// Windows PowerShell 5.1。
    WindowsPowerShell,
    /// PowerShell 7 或更高版本。
    PowerShell,
    /// SSH 客户端能力。
    Ssh,
    /// 串口枚举和读写能力。
    Serial,
    /// 文件上传和下载能力。
    FileTransfer,
    /// TCP 端口探测能力。
    PortProbe,
    /// 指定目标 TCP 受控收发能力。
    TcpExchange,
    /// 进程、服务和系统电源操作能力。
    SystemOperations,
    /// 交互式 Windows 桌面图形能力。
    Visual,
}

/// Agent 已启用能力的有序集合。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet(BTreeSet<Capability>);

impl CapabilitySet {
    /// 创建能力集合。
    #[must_use]
    pub fn new(values: impl IntoIterator<Item = Capability>) -> Self {
        Self(values.into_iter().collect())
    }

    /// 判断指定能力是否启用。
    #[must_use]
    pub fn contains(&self, capability: Capability) -> bool {
        self.0.contains(&capability)
    }

    /// 返回能力迭代器。
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.0.iter()
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        Self::new(iter)
    }
}

/// 可选的 Shell 类型。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    /// Windows CMD。
    Cmd,
    /// Windows PowerShell 5.1。
    WindowsPowerShell,
    /// PowerShell 7+。
    PowerShell,
    /// 平台默认 Shell。
    System,
}
