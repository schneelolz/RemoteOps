# MCP 与 Skill 独立发布规范

适用范围：MCP `0.2.0-preview.20` 起的独立组件发布；Skill 首个独立版本为 `0.1.0`，当前补丁版本为 `0.1.9`。更新日期：2026-11-10。本文规定发布和安装边界，实际构建、自检、安装及宿主加载结果以对应交付记录和摘要清单为准。本批实际更新见 [TCP刷新白屏修复交付](tcp-close-race-fix-20261110.md)。

## 组件职责与版本

| 组件 | 本轮版本 | 职责与更新条件 |
| --- | --- | --- |
| MCP | `0.2.0-preview.30` | 动态全局指引、TCP 策略查询和受控回环转发 |
| 密码安全输入程序 | `0.2.0-preview.7` | 控制端安全窗口；跟随 MCP 包配套交付，清单单独记录实际版本和摘要 |
| Skill | `0.1.9` | 全局指引、在线目标策略和转发验收说明 |
| 协议 | `20` | 由程序实现决定；不随 Skill 文案版本递增 |
| 定制 Agent | `0.4.0-jlai.18` | 持续 TCP 转发及权限初始化修复，未签名候选 |
| Relay | `0.2.0-preview.22` | 在线 TCP 目标策略、授权流路由和权限审计 |
| 公共 Agent | `0.2.0-preview.41` | 持续 TCP 目标连接、背压及默认授权初始化 |

MCP 与 Skill 不要求版本号一致。当前 Skill 最低 MCP 为 `0.2.0-preview.28`（见 [契约说明](../deploy/skills/remoteops/references/tool-center-contract.md)）；最低版本只表示指引所需接口范围，不等于所有目标平台都已通过实测。显式适配需配套新版 Relay 与支持能力的 Agent；协议 19 不代表所有字段均受旧组件支持。安装或自检应分别报告实际版本、兼容要求和验证范围，不能把“文件已复制”写成“宿主已加载”。

MCP 工具描述保留参数语义、关键前置条件和必要的调用边界；完整操作流程以 Skill 为准。权限、加密、主机密钥核验和受控目录等要求继续由程序执行，不依赖模型是否读取 Skill。

## 源码与发布入口

- Skill 唯一公共源码为 `deploy/skills/remoteops`。版本及兼容信息记录在 `skill.json`，`SKILL.md` 的 `metadata.version` 必须与其一致；Windows/macOS 不再维护相互复制的 Skill 正文。
- Skill 独立构建入口为 `scripts/Build-RemoteOpsSkill.ps1`，不调用 Cargo、不读取用户 MCP 配置或凭据。
- Windows MCP 构建入口为 `scripts/Build-WindowsMcpPackage.ps1`；macOS MCP 构建入口为 `scripts/Build-MacosMcp.sh`。MCP 包不包含 Skill。
- Windows 本地交付统一入口为 `scripts/Publish-WindowsMcpDelivery.ps1`。以后向 Y 盘生成 MCP 交付时，同时生成独立 MCP、独立 Skill 和组合安装包，并生成根目录 README 与交付清单；仅修改 Skill 时复用经过摘要和版本核对的现有 MCP 产物，无需重新编译 MCP、Agent 或 Relay。

生成完整交付时显式指定发布目录：

```powershell
.\scripts\Publish-WindowsMcpDelivery.ps1 -OutputRoot 'Y:\MCP\RemoteOps'
```

只更新 Skill 时，先提升 Skill 版本，再复用当前交付清单中的 MCP：

```powershell
.\scripts\Publish-WindowsMcpDelivery.ps1 -OutputRoot 'Y:\MCP\RemoteOps' -SkillOnly
```

发布脚本也接受 `-McpPackageDirectory` 和 `-SkillPackageDirectory` 指定已验证组件包目录。未传组件目录且未使用 `-SkillOnly` 时正常构建两者；`-SkillOnly` 必须读取现有 `delivery-manifest.json` 并重新校验被复用的 MCP，不编译、安装或重启 MCP。所有内容先在本地 stage 完整准备和校验，再将旧输出目录备份到同级 `RemoteOps-backups/<批次>` 后切换；发布失败恢复旧目录。未显式指定 Y 盘时使用本地产物目录，不能把“本地生成完成”写成“Y 盘已发布”。

程序构建必须记录真实源码来源、锁文件及法律文件。组件复用要保留原有构建来源，另记本次组合包来源与 Skill 来源，不能将未重编译的程序冒记为由新的 Skill 源码快照编译。

## 交付目录与内容

Y 盘固定入口为 `Y:\MCP\RemoteOps`，目录按组件自身版本保存：

```text
RemoteOps/
├── README.md
├── delivery-manifest.json
├── SHA256SUMS.txt
├── MCP/<MCP版本>/
│   ├── RemoteOps-MCP-Windows-x64-<MCP版本>/
│   └── RemoteOps-MCP-Windows-x64-<MCP版本>.zip
├── Skill/<Skill版本>/
│   ├── RemoteOps-Skill-<Skill版本>/
│   └── RemoteOps-Skill-<Skill版本>.zip
└── Bundle/mcp-<MCP版本>_skill-<Skill版本>/
    └── 组合安装包及对应目录
```

独立 MCP 包只携带程序、helper、安装/自检脚本、README、许可证和组件清单。独立 Skill 包携带 `skills/remoteops`、独立安装/卸载脚本、README、版本兼容信息和摘要清单，不包含 EXE 或任何运行凭据。组合包包含两个独立组件，方便首次安装，不能将两者重新约束成同一个版本。

根目录 `README.md` 必须说明本批组件版本、适用平台、新安装与已有安装的不同步骤、单独更新 Skill 的方式、完整性校验、卸载边界和重载要求。`delivery-manifest.json` 分别记录 MCP、helper、协议、Skill、兼容要求、各组件包和组合包摘要；每个组件包的文件清单覆盖实际交付内容。

更新现有 Y 盘交付前先保留受限可追溯备份；旧归档保留在明确的历史或备份位置。相同版本内容变化不得静默覆盖：要提升对应组件版本，或拒绝冲突并保留证据。发布包不得混入本机配置、Token、密码、真实控制码、私钥、状态或审计日志。

## 安装与更新

首次安装按组合包 README 先安装 MCP，再安装 Skill。MCP 新安装继续通过设置码或设置文件登记独立身份；已有安装使用升级流程复用原 `controller-config.json` 与系统凭据，不重新登记。具体命令分别以独立包内 README 为准。

Skill 可独立安装、更新或卸载。安装 Skill 不改 MCP 程序路径、连接配置、认证、命令模式、超时或其他 MCP；已有较新 Skill 不得被旧 MCP 包降级覆盖。对本地修改和未版本化旧目录，先检查差异并按独立安装器约定保护原内容，不能把用户改动当作可清理缓存。Skill 备份、暂存与失败证据统一放在技能搜索目录之外的 `remoteops-skill-backups`，避免备份中的旧 `SKILL.md` 被再次发现；显式路径需通过边界及链接检查。

已使用 Skill `0.1.0` 的安装者应核对技能搜索目录内是否遗留 `remoteops.backup-*`，确认归属后将完整旧备份移到搜索目录之外；`0.1.1` 不自动移动任意已有目录。

MCP 可以在未安装 Skill 的宿主运行；安装、自检和卸载不能把 Skill 缺失视为 MCP 本身失败，也不默认复制、覆盖或删除独立 Skill。需要自动化操作流程的宿主应另外安装 Skill。安装器报告的兼容范围不代替实际工具发现和目标验收。

Codex 默认通过标准 Skill 目录发现 `remoteops`，兼容目录按独立 Skill 安装器规则维护；自定义 CodexHome 或 pi 等宿主使用独立安装器提供的目录选项并核对该宿主实际发现路径。不能仅因两个宿主共用 MCP 程序就假定它们自动共用已加载的 Skill。

更新 MCP 配置后，完全退出实际 Codex CLI/桌面/IDE 宿主并重开；pi 重开或 `/reload`，单独 reconnect 不能代替配置重载。仅更新 Skill 文件不需要重启 MCP 进程，但仍要按宿主方式重新加载 Skill 或开启新会话；已运行任务不保证自动读取新正文。

## 发布与验收

发布前至少覆盖：

1. MCP 程序、PE/CLI 与标准 MCP 初始化报告的版本一致；helper 版本、协议、许可证和摘要匹配清单。
2. Skill 元数据、正文和安装脚本的一致性；安装、重复安装、升级、拒绝降级、篡改拒绝以及本地内容保护。
3. 无 Skill 的独立 MCP 安装、自检仍可完成；MCP 更新和卸载不会更改独立 Skill，Skill 更新不会更改 MCP 配置与凭据。
4. 两个独立包与组合包内容一致，Y 盘复制后重新核对递归摘要，README 指向实际存在的安装入口。
5. 分别报告“包已生成”“配置已更新”“独立进程接入验证通过”和“宿主重开后生效”。未完成宿主重开验证时不能将最后一项标成通过。

交换机验收按实际目标记录 TCP、主机密钥核对、凭据解密、认证、命令执行各阶段；一台现场交换机通过只证明该设备的实测范围。交互 PTY、多厂商设备、Linux SSH、macOS 真实安装等未完成项必须单列，不能用本机单测或上传成功替代。

本轮实际结果及未验证项记录在 [MCP 与 Skill 拆分交付记录](mcp-skill-split-delivery.md)。

2026-11-06 用户后续授权的实际安装、生产中继更新和定制 `.11` 双目录交付见 [工具中心配套上线记录](tool-guide-jlai11-rollout.md)；不将前一阶段的“仅源码验收”当作本批部署状态。
