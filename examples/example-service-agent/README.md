# 定制服务 Agent 示例

本产品仅提供资料、策略和薄入口，通过公共 remoteops-agent-service 库复用 SCM、状态、停止、控制台及 Agent 生命周期。独立 workspace 保留自己的 Cargo.lock。示例固定测试端点 relay.example.invalid，不连接生产环境；接入真实环境应修改产品端点后重新构建。

服务和 --console 均强制禁用内建图形能力，环境变量、配置和 FullAccess 无法启用真实图形 Provider。此边界不宣称对任意管理员 Shell 提供操作系统沙箱。

默认机器级目录为 %ProgramData%\ExampleServiceAgent\Agent，普通配置仅保存 managed_enrollment_file 路径。由授权门户部署流程在该独立目录创建凭据文件，内容结构为 target_id 与 token；不要将真实凭据复制进示例、日志或仓库。受管模式先验证凭据，登记成功后保持无码待命，邀请由门户按需签发。

Windows 部署账户必须显式限制配置目录和凭据 ACL，关闭继承，仅给 SYSTEM、Administrators 和实际服务账户所需访问权限。可使用同目录 Protect-AgentDirectory.ps1。运行库不声称自行验证了 Windows ACL；Unix 凭据文件拒绝组和其他用户权限。

正式安装复用[公共安装器](../../deploy/agent-service/windows/README.md)，传入 `-ServiceName ExampleServiceAgent`、独立程序和数据目录以及定制 EXE 路径；公共安装器设置开机自动启动和故障恢复。服务名必须与产品入口一致；下方 `Test-ServiceAcceptance.ps1` 使用手动启动的临时验收服务，不能代替正式部署配置。

构建和仅验证配置（不启动服务、不连接 Relay）：

~~~powershell
cargo build --locked --manifest-path examples/example-service-agent/Cargo.toml
cargo run --locked --manifest-path examples/example-service-agent/Cargo.toml -- --check-config --config C:\ProgramData\ExampleServiceAgent\Agent\agent-config.json
~~~

--console 供安装前诊断，目标上下文准确报告 console，仍保留无图形产品上限。SCM 实际运行报告 windows_service。安装后应分别验收 Session0 无用户登录、有活动 RDP 且图形环境变量开启、FullAccess、重连和重启；本示例不代表已完成这些现场验收。

## 真实 SCM 验收

Test-ServiceAcceptance.ps1 默认只读，不安装、不停止服务，不自动提权、不注销任何用户。只有显式 -InstallAndStart 才在管理员终端创建并启动不存在的测试服务，服务名必须匹配产品内置名；服务专属环境同时设置两个图形开关为 1。先准备真实受管部署凭据及目录 ACL，再安装。

NoLoggedOnUser 场景须在无人交互登录的专用测试机通过获授权的管理通道执行；ActiveRdp 场景须由执行者事先建立并确认活动 RDP。两项均由执行者确认前提，脚本不冒充自动验证登录状态。不要在当前桌面执行注销来凑条件。

通过已认证门户配对目标并明确授权 FullAccess 后，导出该会话的 GetTargetContext details、控制状态和六类合法图形请求结果，再用 -TargetContextFile、-ControlStateFile、-GraphRejectionReportFile 只读核验。拒绝报告为 JSON 数组，每项含 operation 和 error_code，六项均须 product_policy_disabled。缺证据会标记 not_verified，不计为验收通过。

脚本原生检查 SCM 进程处于 Session0；目标上下文必须报告 windows_service、Session0 和 product_policy_disabled。随后人工执行网络重连、重新配对、进程重启及授权撤销场景。实际 Provider 构造/探测/调用 0 由 Agent 自动化计数和合法 duplex 协议测试证明，不能仅凭服务没有弹窗或没有子进程判定。

当前开发机未以管理员运行，未安装或执行真实 SCM/无人登录/RDP 现场验收。
