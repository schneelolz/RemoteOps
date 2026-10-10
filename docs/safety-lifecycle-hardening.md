# 只读边界与执行生命周期加固

本页说明源码修复的行为契约。提交或 CI 通过不代表实际 Agent、Relay 和 MCP 已更新；本文不替代 Windows、真实串口、现场部署及回滚验收。配套控制授权仍以 [控制权限与默认模式](control-permissions.md) 为准。

## 只读查询

- PowerShell 的字面量快捷路径只接受一个完整字符串，不接受拼接、嵌套表达式或无法证明安全的引号/展开。
- `arp`、`ss` 和 `ip` 只接受明确列出的诊断参数/读取子命令；修改、批处理、输出到文件及未知参数不能取得只读待遇。
- 调用方指定的任意绝对、相对、UNC 或模块限定路径不能仅凭文件名取得只读待遇。为兼容现有 IIS 查询，仅保留固定 `C:\Windows\System32\inetsrv\appcmd.exe` 读取路径。裸程序名仍依赖目标机可信的 PATH、当前目录及命令解析环境；本次未引入可执行文件签名或系统路径固定。
- 风险扫描识别命令位置，避免把 `Format-Table`、`Format-List` 或普通文件名误判成磁盘格式化/关机。格式化管道、脚本块、多语句等仍不属于只读语法；修正高风险误报不表示放行复合脚本。无法可靠解析的风险语法保持保守审批。
- 四种权限模式、Human/AI、Agent 执行前复核和结构化工具边界保持不变。未知命令不会自动改用其他工具或提高权限。

## 持久 Shell 与连接资源

- 从等待会话锁开始使用同一个执行期限；写 stdin、读取 stdout/stderr 和等待退出均受该期限及取消约束。
- 两个输出流并行读取固定大小字节块，共享 4 MiB 原始输出预算。无换行、分片 UTF-8 和终端控制序列不能绕过预算。
- 两个流各自确认完成边界；普通输出不依赖行末换行。PowerShell 终止性错误产生失败退出码与完成边界。显式 `exit` 仍关闭会话。
- 子进程由独立监督任务持有。中断不等待命令 I/O 锁；放弃已派发的执行 future 也会清理进程树。
- 已关闭或失败的会话及时从 Agent 配额移除，错误响应可携带 `shell_closed`。旧请求清理不会移除后来登记的资源。
- Relay 连接结束会关闭持久 Shell 和串口会话，串口 reader 完成后释放句柄。逻辑 Agent 身份可恢复，设备会话须重新打开，不重放写操作。串口正常驱动读取超时为 200 ms；真实驱动行为仍需现场验证。

## 文件提交与取消

- `overwrite=false` 使用同文件系统硬链接的原子 no-replace 发布，再移除临时名字。竞争写入只能有一个成功者；不支持硬链接的文件系统返回错误，绝不回退到可能覆盖目标的 rename。
- 普通上传和分块完成的 blocking worker 在写入块、哈希块及发布前检查取消。取消与不可逆发布共享一个提交状态。
- 取消先获胜时阻止目标发布，并等待已启动 worker 清理后才报告已取消。发布先获胜时等待真实成功/失败结果，不能把已发布文件报告为已取消。
- 已完成发布不回滚。允许覆盖模式保留现有备份/恢复行为；本次不宣称它具有跨文件系统事务、断电恢复或对任意外部路径置换的保证。

## Controller 与 Relay

- 每次 TCP/TLS/欢迎消息连接尝试共享 20 秒预算，重连同样受限。Controller 心跳 ACK 45 秒不再新鲜会关闭整条 transport 并进入原有重连流程；不能取消半帧读取后复用该字节流。
- Relay 过期 Controller 连接按租约清理，保留现有连接代次/Owner 校验。
- `PendingOperation` 的等待超时或调用方放弃会移除本地 pending 登记。业务超时仍不等于远端取消：MCP 返回 `status: unknown`、原 `request_id`、`cancellation_confirmed: false` 和 `safe_to_retry: false`。先核查/显式取消，不自动重试写请求。
- Human/AI 双绑定中一方离开且另一方仍在时，已开始操作保留原始归属直到终态，使同 Owner 的 Human 仍可按请求取消。旧结果不交付给重连后的新代次。撤销不会因此强行中断已开始操作；最后一个绑定移除沿用既有 Agent 清理语义。

## 验证与未覆盖事项

回归使用纯字符串策略矩阵、临时文件、本机受管进程、Tokio 可控时钟和模拟协议/串口；不向生产主机或网络设备发送探测性操作。

推荐验证：

```sh
cargo fmt --all -- --check
cargo check --workspace --exclude remoteops-agent-gui --exclude remoteops-controller-gui --locked
cargo clippy --workspace --exclude remoteops-agent-gui --exclude remoteops-controller-gui --all-targets --locked -- -D warnings
cargo test --workspace --exclude remoteops-agent-gui --exclude remoteops-controller-gui --locked
```

Windows PowerShell 5.1、PowerShell 7、Windows 进程树和真实串口须在对应环境验证。GitHub Windows/macOS/Linux 工作流的结论应以相应提交的实际结果为准。

仍需单独决策/验证：开放新 Agent 登记的配额/保留策略（须兼容丢失确认后的合法恢复）；真实网络设备对编辑控制字节的处理；MCP 宿主取消通知与远端取消的完整桥接；固定可信系统可执行文件身份。未将这些事项描述为已修复或现场已验收。
