# UI、SSH 与 MCP 合并修复交付记录

日期：2026-11-05。用户本批明确授权同步定制项目、更新生产 Relay、更新 pi/Codex MCP，并按既有位置交付定制版。未执行 Git 写操作；保留工作区原有改动。正式产品固定 Git rev 未变，本次交付为可追溯源码快照的 Win32 候选。

## 版本与变更

| 组件 | 本批版本 |
| --- | --- |
| Agent / desktop-host / Win32 | `0.2.0-preview.33` / `0.3.2` / `0.5.2` |
| 定制 Win32 候选 | `0.4.0-jlai.10` |
| MCP / Skill / Relay | `0.2.0-preview.21` / `0.1.2` / `0.2.0-preview.13` |
| 协议 / credential-prompt / askpass / Plink | `19` / `0.2.0-preview.6` / `0.2.0-preview.7` / `0.83`（保持） |

MCP 两个工具中心工具使用四字段明确输出类型；根为 object，error 必需且可空，data 使用空对象 Schema 允许任意 JSON。结构化和文本 JSON 结果保持一致。增加真实 router 离线检查入口 `--check-tool-schemas`，安装与自检在读取配置、凭据和联网前执行。测试固定官方 SDK 1.32.2、Ajv 8.17.1；未升级 rmcp、未向领域库引入 Schema 依赖。

Win32 两个公共入口共用生命周期显示：有界事件、三行最近记录、按任务计数脉动、重连重绘、明确颜色与文案、长文滚动和暂停、遵循系统动画设置，隐藏/最小化停止动画绘制。操作实况继续收集有界历史，仅手动打开。

Windows 密码 SSH 使用随包 Plink；扫描器固定为微软官方 OpenSSH `10.4.0.0p1-Preview`，附 libcrypto.dll、LICENSE、NOTICE。ZIP SHA-256 为 `b67ff8becd203502190ec42ac7787c24c2cd3325ed7700485fe7a1c83d2a7957`；摘要、微软签名、PE 依赖及隔离启动门槛通过。未更改系统 PATH 或系统 OpenSSH。保留完整公钥、精确端点、多指纹比对、信任前重扫及原子写入。默认 15 秒总截止贯穿扫描和保存，锁等待最多 2 秒；超时/取消终止实际扫描进程。同步文件系统调用本身不能被强制中断，返回后复查截止；已成功的原子替换按真实成功返回。

SSH 返回实际后端、选择理由、exec/无 PTY 模式及可确认错误类别；不确定阶段标 unknown，保留退出码且不自动切换客户端重放。三端共享只读分类器，精确允许大小写敏感的 hostname、whoami、uname -a，先检查原始命令注入再解析空格，不改写命令。Skill 唯一源码 `deploy/skills/remoteops`，最低 MCP 为 `.21`。临时兼容准备后成功的历史报告未计作本批现场验收。

## 来源与产物

最终批次：`C:\Temp\RemoteOps-ui-ssh-mcp-20261105\build-final3`。公共 306 文件内容摘要 `d567bde8e5659a169f2542a115594c925505e7d15d027c2942a7fa431d6b989e`；产品输入摘要 `f45f04175c3e4beea2778dfc807881337cd7bdd12b840995e026a266a14120be`。`build-final`、`build-final2` 为预检证据，不用于交付。冻结后只补验收脚本及交付文档；补充脚本单独归档，未改编译输入或冒记来源。

定制 EXE：`精灵AI-Agent-Win32-0.4.0-jlai.10.exe`，11,244,032 字节，SHA-256 `eaf0f1e9bc62caa1ee60ca77aeb02a144fe1764dad3d4db524ded86bf2d32a52`。仍为未签名候选。

最终 MCP EXE SHA-256 `3481cfda3b959379eb953d7524e908b56d99bcb39ca3f032d634c9b0940d7f38`；Linux x64 Relay SHA-256 `3c1925c4dcb3044f4ac44eb649b6c5816c583535bd7f19ac5d18ab6d440d232a`（glibc 2.36）。

## 构建与验证

| 命令或检查 | 实际结果 |
| --- | --- |
| `cargo test --offline --locked --workspace --lib --bins -- --test-threads=1` | 995 通过，0 失败，6 忽略 |
| `cargo check --offline --locked --workspace --all-targets` | 通过 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | Windows 通过 |
| `cargo fmt --all -- --check` | 通过 |
| 最后 device/policy/desktop/native 边界测试 | 290 通过 |
| 定制候选构建中的格式/check/clippy/test | 通过；11 个产品测试通过 |
| Windows Release-small 配套与定制单 EXE、Linux x64/glibc 2.36 | 构建通过 |
| `npm test --prefix tests/mcp-tool-schemas`（指定最终 Release MCP） | SDK/Ajv 14 项通过，含分页、注解、名称唯一及负例 |
| `node tests/mcp-tool-schemas/live.mjs --mcp ... --relay ... --output ...` | 隔离真实初始化/发现/调用 10 项通过；默认 44、测试 46 工具；协商 MCP `2025-11-25` |
| `python scripts/Test-McpSetupInstallers.py ...` | 最终 Windows 50 项，0 失败、3 条件跳过；早期失败日志不作为最终结果 |
| `python scripts/Test-McpSkillDelivery.py --mcp ... --skill ... --powershell pwsh.exe` | 最终包 13 项通过 |
| Skill 安装/升级测试及 quick_validate | 两种 PowerShell 各 19 项通过，正文验证通过 |
| `Test-JlaiWin32Candidate.ps1 ... -ManifestPath ...` | 181 项通过、86 子进程；来源、全部嵌入摘要、释放及篡改拒绝通过，无窗口观察 |
| `Test-JlaiExecutableIcon.ps1 ...` | 30 项通过、7 帧图标解码 |
| `Test-AgentGuiConsole.ps1 ...` | CLI help/version/坏配置 3 项通过；最后一项依赖旧 GUI 专属日志，对定制原生入口探针不适用，不记为全通过 |
| Relay 管理端 Node 测试 | 5 项通过 |
| SSH/传输完整隔离端到端 | 55 项通过；真实 Plink、三条精确只读查询、Unicode、换钥/错密、取消/凭据清理通过 |

离线 Rust 检查覆盖实际生成 Schema/引用规则，不是通用元 Schema 引擎；完整合法性另由 Ajv2020 验证。Linux 构建有既有资源库 unused/dead_code 警告，未声称 Linux 严格 clippy 通过。

交互桌面用 Computer Use 的 sky 接口复核首次暗色、中文浅/暗、英文浅色、暂停/恢复文案、手动打开与关闭操作实况，截图留档。演示专属英文长状态有裁切；实际生产英文状态按同字体 100%/150%/200% 内存测量可容纳。此测量不代替真实 DPI 桌面验收。

## 生产切换与本机升级

生产 Relay 镜像已切为 `remoteops-relay:0.2.0-preview.13-p19-jlai10-20261105`，镜像 ID `sha256:739bcd2ad75dc6195475d0d96f8c43d43c6ae06d7831a9119be2868e98da8831`。仅重建 Relay 服务；健康、运行用户/端口/数据卷保持，重启 0 次，启动日志 ERROR/panic 计数为 0，公网 7443 TLS 1.3 证书校验通过。

停服后完整备份 Compose、env 和数据卷（含 JSON、SQLite、WAL），仅留远端 `/opt/remoteops/backups/remoteops-before-preview13-p19-jlai10-20261105`，目录 700，机密文件及 data.tar 为 600。隔离无网副本读库验证完整性正常，MCP schema 仍为 2、授权记录数保持、工具管理授权仍为 1；工具库保持空且执行关闭。失败回滚脚本与旧镜像配置随备份保留。

Codex 与 pi 只修改 RemoteOps command 路径，指向并排安装的 `protocol19-preview21-3481cfda3b95-b5ce1dba068d`。原身份配置字节、系统凭据、其他 MCP、模式与超时保持。配置完整备份位于 CodexHome `remoteops/backups/ui-ssh-mcp-jlai10-20261105`，不纳入交付包。Skill 0.1.2 已独立安装到标准 `.agents/skills/remoteops`、自定义 CodexHome 兼容目录及 pi 技能目录，旧内容备份放搜索路径之外。

切换前后分别用新的独立 MCP 进程、固定 SDK/Ajv 完成初始化、严格发现及只读 snapshot：版本 `.21`、44 工具、管理权可读、工具执行关闭。独立验证进程均已关闭，未启动现场任务。

Y 盘统一经 `scripts/Publish-WindowsMcpDelivery.ps1` 发布：`Y:\MCP\RemoteOps`，包含独立 MCP、独立 Skill、组合包、README、清单与摘要，52 文件通过校验。原交付完整备份为 `Y:\MCP\RemoteOps-backups\20261105-182948-9be77eb2`。

## 定制版交付及证据

同一 EXE 已复制到以下新目录，并再次计算两份 SHA-256；原 `.9/.8/.7` 六份文件摘要全部保持：

- `C:\Users\ywb\Desktop\RemoteOps\0.4.0-jlai.10`
- `Z:\RemoteOps\0.4.0-jlai.10`

完整源码快照、锁文件、第三方包/许可、配套程序、日志、截图与摘要归档于 `D:\Projects\Other\RemoteOps-JLAI\artifacts\win32-single\0.4.0-jlai.10-20261105`。最终验收 SSH 测试修正了既有 details 元数据帧的精确断言及 Paramiko 夹具 exec-success 先于输出的竞态；两份测试源码与定制 Manifest 校验补充脚本另存 `verification-source`，未改已冻结产品二进制。早期失败测试保留在本批 C 盘目录，最终判据为 `transfer-ssh-e2e-final-complete.json`。

主要修改涉及 MCP 的 `tool_center_output.rs`、`tool_schemas.rs` 与主入口/安装器，公共 `lifecycle.rs`、`activity_view.rs` 和两个 Win32 入口，device 扫描/可信工具/原子文件、共享 policy 与 SSH 返回诊断，独立 Skill、定制薄入口资源及构建/校验脚本。各组件来源与递归文件摘要见归档清单。

## 待验与生效边界

真实 Linux/H3C 设备的首次/已有信任、安全密码、换钥、错误密码和三条只读命令，Server 2016、真实 100%/150%/200% DPI、多任务动画连续事件、真实串口设备、macOS 实机安装仍需现场验收。隔离夹具的 Linux 风格 SSH 回应不等于 Linux 设备现场通过。

当前 Codex/pi 宿主未重启，不声称当前会话已加载新 MCP/Skill。Codex 必须完全退出并重开；pi 重开或 `/reload` 后生效。用户后续使用本批定制版测试，现场结果应追加记录，不覆盖本批可复现证据。
