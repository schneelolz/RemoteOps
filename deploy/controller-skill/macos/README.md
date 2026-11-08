# RemoteOps Skill 的 macOS 安装

公共源码为 `deploy/skills/remoteops`，版本由 `skill.json` 与 `SKILL.md metadata.version` 共同校验。使用 `scripts/Build-RemoteOpsSkill.ps1` 生成的平台无关完整 Skill ZIP；该包的根目录 README 同时包含 Windows/macOS 安装、升级、卸载和兼容说明。

macOS 需 Python 3.9 或更新版本。在解压后的完整 Skill 包中执行：

```bash
bash ./install-remoteops-skill.sh
bash ./install-remoteops-skill.sh --skill-directory "$HOME/.pi/agent/skills/remoteops"
bash ./uninstall-remoteops-skill.sh
```

自定义 Codex 使用 `--codex-home PATH`；确认迁移旧 MCP 附带的未版本化 Skill 时添加 `--replace-unversioned`，旧目录完整备份。更新版本不会降级或覆盖已有本地修改。Skill 安装与卸载不触碰 MCP 配置、凭据和进程；新建宿主会话读取更新，pi 重开或 `/reload`。

本轮只能验证公共 Python 逻辑和 shell 语法；真实 macOS 宿主行为待在 Mac 上验收。
