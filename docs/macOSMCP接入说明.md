# 在 Mac 上构建 RemoteOpsBridge 和 MCP

以下命令在仓库根目录执行。先用 `git branch --show-current` 和 `git rev-parse --short HEAD` 核对当前检出的分支和提交。两个组件都通过仓库的 Cargo.lock 固定依赖。

## 编译 RemoteOpsBridge

安装 Xcode Command Line Tools 和符合根目录 [Cargo.toml](../Cargo.toml) 要求的 Rust 工具链，执行：

```bash
cargo build --release --locked -p remoteops-bridge
```

默认输出为 `target/release/RemoteOpsBridge`。宿主通过标准输入和标准输出与它交换 JSON-RPC 消息。在 macOS 上，宿主初始化时应显式传入当前用户可写的绝对目录 `dataRoot`，例如用户主目录下的 `Library/Application Support/RemoteOpsBridge`（由宿主展开成绝对路径）。省略该字段会查找 Windows 的数据目录环境变量。构建与宿主联调是两项验收，联调参数见 [RemoteOpsBridge 接入说明](integrations/remoteops-bridge.md)。

## 生成 Apple Silicon MCP 安装包

构建机使用 macOS 13 或更高版本的 Apple Silicon Mac，准备 Rust、rustup、Python 3、PowerShell 7（`pwsh`）和 Xcode Command Line Tools。选择一个新的输出目录：

```bash
bash scripts/Build-MacosMcp.sh "$PWD/artifacts/mac-build"
```

脚本读取 MCP 和凭据辅助程序各自的 Cargo 版本，编译 arm64 程序并检查架构和版本。安装包位于 `artifacts/mac-build/RemoteOps-MCP-macOS-arm64-<版本>.tar.gz`。解压后进入包目录可用 `shasum -a 256 -c SHA256SUMS.txt` 校验文件。

## 登记和验证

管理员签发一次性设置码或设置文件，安装器将此 MCP 安装登记到指定 Relay。安装后的自检、重载宿主、升级和卸载步骤见 [macOS 安装包说明](../deploy/controller-mcp/macos/README.md)。登记流程和设置码的用途见 [MCP 一次性安装配置](mcp-one-time-setup.md)。
