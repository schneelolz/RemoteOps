# RemoteOps Agent Windows Service

这是被控端的可选后台宿主，不改变 `remoteops-agent` 核心运行逻辑和权限模型。

## 安装

先准备不含真实 Token、密码或私钥的 Agent 配置文件，然后使用管理员 PowerShell：

```powershell
.\Install-RemoteOpsAgentService.ps1 `
  -ConfigPath .\agent-config.json `
  -StartService
```

首次登记不需要入网码或部署级 Agent 注册 Token。服务只需读取包含 Relay 地址和 TLS 信任配置的 `agent-config.json`；首次登记成功并生成 `agent-state.json` 后，后续重连使用恢复令牌。Service 没有证书确认界面，公网 CA 可直接使用系统可信根，自签名或私有 CA 必须由管理员在配置中预置 `ca_cert` 或已核对的 `tls_fingerprint`。

默认使用 `NT AUTHORITY\LocalService`，配置、身份状态、传输目录和短期状态位于 `C:\ProgramData\RemoteOps\Agent`。只有明确的实验环境才应使用 `-AllowLocalSystem`。

服务运行在 Session 0，不能操作用户桌面、映射盘或仅安装在某个用户目录中的工具。需要交互式现场协助时，使用 `remoteops-agent-gui.exe`；需要开机后台运行时，使用本服务。

## 状态和卸载

```powershell
.\Get-RemoteOpsAgentStatus.ps1
.\Uninstall-RemoteOpsAgentService.ps1
```

卸载默认保留配置、Agent 身份状态和传输目录；只有明确指定 `-PurgeData` 才清理数据。
