# Example Assist

中性薄产品示例，固定 `relay.example.invalid:7443`，默认完全控制，启用公共四行活动摘要。它不会连接生产环境。产品只有配置、资源、薄启动入口及一个连接提示扩展；公共代码全部通过依赖复用。

```sh
cargo build --locked --manifest-path examples/example-assist/Cargo.toml
cargo run --locked --manifest-path examples/example-assist/Cargo.toml -- --demo
```

真正的 workspace 外构建验证：

```sh
python3 scripts/Test-ExternalProduct.py
```

初始化或升级示例锁文件时显式添加 `--update-lock`，普通验证不更新锁文件。外部项目应保留自己的锁文件与构建设置；path 依赖改为统一 Git rev 的具体方法见[产品接入文档](../../docs/product-integration.md)。

`--check-config` 只检查公共启动解析，不打开窗口、不连接网络。`--build-info` 输出产品版本及实际公共库/协议/来源提交信息。Windows 的 EXE 图标与产品资源由自己的 build.rs 调用公共资源工具嵌入。

示例图标是仓库内 SVG 几何图形，PNG/ICO 为其位图表示，不包含私有品牌素材。
