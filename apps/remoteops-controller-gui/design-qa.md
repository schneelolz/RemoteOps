# Controller GUI 设计验收

## 目标

- Windows 原生窗口在浅色与深色主题下保持清晰层级。
- 左侧连接栏与右侧工作区遵循一致的间距、圆角和控件高度。
- 同一层级按钮的文字、图标和点击区域保持居中与对齐。
- 小窗口下不遮挡连接状态、审批入口和输入区域。

## 必测状态

- 无连接、单连接和多连接；
- AI 只读查询、直接命令、等待审批、批准、拒绝和取消；
- 长命令、长输出、错误输出和可展开工具详情；
- 串口工作台的连接、断开、只读授权和查询结果；
- 浅色、深色和高 DPI；
- 最小窗口尺寸与常见 16:9 分辨率。

## 验收方式

```powershell
cargo test -p remoteops-controller-gui --locked
cargo clippy -p remoteops-controller-gui --all-targets --locked -- -D warnings
cargo run -p remoteops-controller-gui -- --demo
```

视觉截图和本地原型属于开发产物，不应在源码文档中记录个人绝对路径或客户机器名称。

