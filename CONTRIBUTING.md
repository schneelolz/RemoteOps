# 参与贡献

感谢你帮助改进 RemoteOps。提交变更前，请先阅读 [安全模型](docs/安全模型.md) 和 [项目结构规范](docs/项目结构规范.md)。

## 开发环境

- Rust 工具链应满足根 `Cargo.toml` 中的 `rust-version`。
- Windows GUI、PowerShell 和文件版本验证需要 Windows 环境。
- Relay Docker 构建需要 Docker Engine 或 Docker Desktop。

## 提交流程

1. 为缺陷或功能建立清晰的问题描述，安全漏洞不要创建公开 Issue。
2. 保持核心业务位于 `crates`，CLI、GUI、MCP 和服务项目只负责宿主与表现层。
3. 为公共行为、边界条件和缺陷修复增加测试。
4. 不提交 Token、密码、证书、私钥、真实控制码、状态文件、审计日志或客户信息。
5. 更新受影响的文档和 `docs/功能索引.md`。

提交 Pull Request 前运行：

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
.\scripts\Test-Documentation.ps1
```

## 许可证

提交代码即表示你有权提供该贡献，并同意该贡献按照项目的 `AGPL-3.0-only` 许可证发布。
