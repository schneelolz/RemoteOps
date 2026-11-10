# RemoteOps Skill 独立安装说明

Skill 是 AI 使用 RemoteOps 的操作指引，包含交换机 SSH、主机指纹核验、权限和结果验收流程；MCP 程序提供实际工具。两者独立版本、独立更新。本包不包含 MCP EXE、Controller 配置或凭据，不会重新登记安装身份。

本包的 `skills/remoteops/skill.json` 是 Skill 版本与兼容要求的事实来源，`manifest.json` 和 `SHA256SUMS.txt` 记录包内文件摘要。Skill `0.1.9` 要求 MCP 至少为 `0.2.0-preview.28`、协议 20，并使用元数据列出的工具。本次仅有真实 Windows/H3C 交换机验收证据，不能推断 Linux 或所有网络设备已通过。

## Windows 安装与更新

解压完整包，在该目录执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-RemoteOpsSkill.ps1
```

默认安装到用户标准 `.agents\skills\remoteops` 及 Codex `.codex\skills\remoteops` 兼容目录。设置 `CODEX_HOME` 或显式 `-CodexHome` 时，安装到该目录下的 `skills\remoteops`；自定义目录不会写其他用户目录。

```powershell
.\Install-RemoteOpsSkill.ps1 -CodexHome 'D:\CodexData'
.\Install-RemoteOpsSkill.ps1 -SkillDirectory "$env:USERPROFILE\.pi\agent\skills\remoteops"
```

`-SkillDirectory` 为精确目标目录，必须以 `remoteops` 命名，可用于 pi 或其他支持 Skill 的宿主；不能与 `-CodexHome` 同用。首次迁移旧 MCP 安装的未版本化 Skill 时，确认后使用 `-ReplaceUnversioned`。原目录完整备份到目标目录的祖父目录下 `remoteops-skill-backups/backup-*`，例如 `.agents/skills/remoteops` 对应 `.agents/remoteops-skill-backups`；已有受管版本升级同样备份。备份、失败证据（`failed-*`）及暂存（`stage-*`）均放在该固定目录，位于所有本次目标 Skill 搜索目录之外，并拒绝链接路径。宿主不会因这些备份重复发现旧版 Skill；安装器不会删除备份。

安装器校验包内清单、摘要及版本，拒绝降级、同版不同内容、受管文件本地修改及链接目录。拒绝时保留原目录；需要保留的本地定制应先人工整理到独立 Skill，不要直接覆盖。

安装 Skill 不要求 MCP 已安装，也不执行 MCP 或检查运行中工具能力；显示兼容要求不代表兼容验收已通过。更新后在 Codex 新建会话，使宿主重新读取 Skill；pi 重开或 `/reload`。仅更新 Skill 不需要重启 MCP 进程；更新 MCP 程序后仍须重开 Codex CLI，pi 重开或 `/reload`。

## macOS 安装与更新

需 Python 3.9 或更新版本；脚本在修改文件前检查该依赖。解压后执行：

```bash
bash ./install-remoteops-skill.sh
bash ./install-remoteops-skill.sh --codex-home /path/to/codex
bash ./install-remoteops-skill.sh --skill-directory "$HOME/.pi/agent/skills/remoteops"
# 仅在确认迁移旧的未版本化目录时添加 --replace-unversioned
```

默认目录和保护规则与 Windows 相同。macOS 脚本语法与公共 Python 安装逻辑可在其他平台验证；真实 macOS 宿主加载及原生路径行为需在 Mac 上另行验收。

## 独立卸载

```powershell
.\Uninstall-RemoteOpsSkill.ps1
.\Uninstall-RemoteOpsSkill.ps1 -SkillDirectory "$env:USERPROFILE\.pi\agent\skills\remoteops"
```

```bash
bash ./uninstall-remoteops-skill.sh
bash ./uninstall-remoteops-skill.sh --skill-directory "$HOME/.pi/agent/skills/remoteops"
```

卸载只删除独立安装器登记且摘要未变的受管文件，保留额外文件和所有备份。未版本化或已修改目录拒绝卸载。MCP 程序、配置、凭据、SSH 主机信任、审计及交换目录均不受影响。

## 发布维护

公共源码仅位于 `deploy/skills/remoteops`。修改指引时提升 `skill.json` 和 `SKILL.md metadata.version` 的 Skill 版本，运行 `scripts/Build-RemoteOpsSkill.ps1 -OutputRoot <新目录>`。不需要 Cargo、编译 Agent 或修改 MCP 版本。新增 MCP 接口依赖时同步调整 `minimum_mcp_version`、协议和 `required_tools`，并完成相应兼容验收。
