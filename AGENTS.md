# MCP 与 Skill 发布约定

涉及 MCP／Skill 构建、安装、版本或 Y 盘交付时，先读 [独立发布规范](docs/mcp-skill-release.md)。

- Skill 唯一源码为 `deploy/skills/remoteops`，版本独立于 MCP；操作指引更新不要求重编译 MCP、Agent 或 Relay。
- 向 `Y:\MCP\RemoteOps` 发布统一使用 `scripts/Publish-WindowsMcpDelivery.ps1`，同时提供独立 MCP、独立 Skill、全套包、根 README 和各组件版本／摘要清单。
- 只有 Skill 更新时提升 Skill 版本并使用 `-SkillOnly`，校验复用现有 MCP；不要重新编译程序或覆盖已安装的较新 Skill。
- MCP 安装、升级、自检和卸载不得强制安装、覆盖或删除独立 Skill；程序的权限与加密校验仍由代码执行。
- 更新发布先完整备份，保留用户配置、凭据和未提交改动；区分文件／配置已更新、独立进程验证通过和宿主重新加载后生效。
