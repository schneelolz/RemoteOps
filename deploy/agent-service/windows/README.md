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

上段为通用 `AutoPerBoot` 产品流程。定制受管产品使用公共宿主并显式选择 `.managed()`，需要独立资产登记证明，上线默认无码；接口和凭据部署规则见[定制服务接入](../../../docs/integrations/custom-service-agent.md)。

三个脚本均接受 `-ServiceName`，安装器还接受 `-DisplayName`，默认值保留原通用服务。定制服务名必须与产品 `ServiceOptions::new` 一致，同时传入独立 `-InstallRoot`、`-DataRoot` 和定制 `-ServiceExecutable`。安装器复用自动启动和 SCM 故障恢复设置，不需要复制脚本；状态查询同时指定准确 `-StatusFile`。安装器不会自动分发登记证明，安装前将文件安全部署到配置引用的绝对路径并设置服务账号读取 ACL。

默认使用 `NT AUTHORITY\LocalService`，配置、身份状态、传输目录和短期状态位于 `C:\ProgramData\RemoteOps\Agent`。只有明确的实验环境才应使用 `-AllowLocalSystem`。

服务运行在 Session 0，不能操作用户桌面、映射盘或仅安装在某个用户目录中的工具。需要交互式现场协助时，使用 `remoteops-agent-gui.exe`；需要开机后台运行时，使用本服务。

## 状态和卸载

```powershell
.\Get-RemoteOpsAgentStatus.ps1
.\Uninstall-RemoteOpsAgentService.ps1
```

卸载默认保留配置、Agent 身份状态和传输目录；只有明确指定 `-PurgeData` 才清理数据。

