# 资源工具验证记录

## 验证范围与结论

共享资源模块和两个真实后端已在本机 Windows 11 build 26200 x64 完成独立 headless 软件验证。当前机器锁屏，测试显式在测试内关闭桌面要求并增加 headless 参数；这不等于正常交互桌面验收。发布清单仍将 Windows 10、11、Server 2019/2022/2025 标为 `pending`，Server 2016 标为 `unsupported`。没有 `passed` 平台时正常启动会明确拒绝。

真实测试使用本地 HTTP 表单夹具，逐后端验证导航、快照、输入与按钮交互、跨会话调用和停止拒绝、已提交操作超时或取消后禁止重放、profile 删除，以及 WMI 与 Get-Process 双重核实自有 Node/Chrome/agent-browser 进程没有存活。Playwright 采用调用超时，agent-browser 采用取消正在等待的 Future；两者均将旧实例标识判定为不可重放。没有访问客户浏览器或沿用现有登录状态。

## 已执行检查

- `cargo clippy -p remoteops-resources --all-targets --locked -- -D warnings`：通过。
- `cargo test -p remoteops-resources --locked --target-dir target/resource-core-verification`：12 通过，0 失败，1 条真实候选测试默认忽略。
- `cargo test -p remoteops-resources --locked --target-dir target/resource-core-verification real_candidates_headless_navigation_state_isolation_and_unknown_results -- --ignored --nocapture`：1 通过，0 失败，耗时 430.65 秒；此条为加入内嵌许可证前的中间候选记录，两个后端均输出 profile 与进程回收通过。最终 ZIP 的严格核实结果另见下文。
- HTTPS 夹具验证两个不同来源切换、ETag/304、重定向最终地址作为相对基址、跨来源拒绝与显式允许、下载大小与摘要、缓存展开树损坏；ZIP 覆盖链接、路径越界、大小写别名和膨胀。
- 取消回归覆盖两会话启动隔离，以及连续十次已提交调用取消后不占实例槽、旧实例禁止重放。

## 打包与许可证

固定输入及其 SHA-256 见 `scripts/resource-inputs.lock.json`。发布机脚本不执行 npm install 或安装脚本，不查询 latest。Node 采用官方 SHASUMS256，npm tarball 采用 SHA-512 integrity；浏览器固定 URL 的首次摘要写入输入锁，后续变化拒绝。

CfT 官方 ZIP 没有独立 LICENSE 文件。发布脚本直接读取固定归档的 DataPack v5：`resources.pak` 资源 41071（Chromium Brotli）为完整 credits，`locales/en-US.pak` 资源 35410（gzip）为 Chrome 附加条款。用固定 Node 内置 zlib 解压 Brotli，无新增 Python 依赖，无须启动浏览器。包内 `LICENSES` 保留 HTML、文本和 proof；proof 包含浏览器归档、源 DataPack、解压 HTML、文本的 SHA-256 及资源 ID。附加条款所引用的 Google 主条款仍为外链，不宣称已经保存固定版本主条款。

早期采集内部网页的实验存在超时，已完全移除该发布路径；最终脚本只启动短时 Node 解压，不启动 Chrome。历史候选保留在 artifacts 下供诊断；交付只使用 final-site 清单引用的包。

## 最终包实测与进程核实

加入内嵌许可证后的最终 ZIP 首轮补测中，Playwright 导航、快照、表单、未知结果门禁与 profile 删除完成后，进程存活断言检测到 1 项，测试退出 1（278.72 秒），未进入 agent-browser 分支。随后只读检查未发现该临时包路径的存活进程。反向顺序补测先验证最终 agent-browser 包通过，Playwright 的旧断言再次误计 2 项（430.23 秒）。严格核实器随后取得证据：本次 Playwright 的 Chrome PID 83664、61564 仍可通过 Get-Process 取得对象及路径，但 `HasExited=true`，创建时间分别为 `2026-11-04T17:53:04.1391980Z` 与 `2026-11-04T17:53:04.6772200Z`；真实存活计数为 0。旧断言把已退出的进程对象误计为存活。此后 Playwright 完整验证通过，进程核实耗时 2.128 秒。原始失败记录保留，避免用较早成功结果覆盖失败。

已把真实进程核实器补成明确读取 `HasExited`、统一 Win32 的长路径前缀，并在最多 10 秒内核实已请求终止的进程退出；保留 PID、父 PID、创建时间、路径与退出状态。没有把仍存活的进程过滤掉，也没有修改运行时代码。修正后的默认资源测试重新通过 12 项，clippy 严格检查与全局格式检查通过。早期默认测试与前次真实结果保持单独记录。

最终命令 `cargo test -p remoteops-resources --locked --target-dir target/resource-manifest-verification real_candidates_headless_navigation_state_isolation_and_unknown_results -- --ignored --nocapture` 退出 0：1 通过、0 失败，325.52 秒。两个最终 ZIP 均通过导航、快照、表单交互、跨会话拒绝、未知结果不重放、profile 回收和真实进程退出核实；agent-browser 核实耗时 1.428 秒。完整日志为 `artifacts/resource-candidates/final-real-test.log`。最终测试只修正核实器，未修改运行时代码。

## 最终候选产物

最终静态站根目录为 `D:/Projects/Other/RemoteOps/artifacts/resource-candidates/final-site`，清单为该目录的 `remoteops/v1/index.json`。目录只包含 README、清单、Schema、协议和清单引用的两个包；没有复制历史候选。源码样例清单与最终清单一致。

| 后端 | 最终 ZIP 字节数 | SHA-256 |
| --- | ---: | --- |
| Playwright MCP 0.0.84 | 258689061 | `6367bf1b69220db4663a11595dc4c710af7af9c80b72ddb1bfcd7558ea9946da` |
| agent-browser 0.41.2 | 225329722 | `499683b78ec6dc3b57e0ba096eed4e26924f67400399585da65378fb36ba7b0a` |

连续两次运行 `python scripts/build-resource-packages.py` 均退出 0；两次完整生成的清单、ZIP 字节数和 SHA-256 完全一致，且与 final-site 交付副本一致。固定输入锁与源码锁文件一致。最终 ZIP 已复核清单字节数、SHA-256、包内锁文件和每份许可证 proof 的源文件/HTML/文本摘要。`python -m py_compile scripts/build-resource-packages.py`、Rust `resource-manifest-check` 对样例与最终清单均通过。使用临时验证依赖 jsonschema 4.25.1 对 Schema 本身及两份清单做 Draft 2020-12 验证通过；未知主版本、未知接口和无效摘要的拒绝检查通过。该验证依赖仅位于忽略的 artifacts 目录，未新增项目依赖。

| 平台与运行方式 | Playwright MCP | agent-browser |
| --- | --- | --- |
| Windows 11 build 26200 x64，独立 headless | 通过软件验证 | 通过软件验证 |
| Windows 11，正常交互桌面 | 待测 | 待测 |
| Windows 10 | 待测 | 待测 |
| Windows Server 2016 | 不支持 | 不支持 |
| Windows Server 2019/2022/2025 | 待测 | 待测 |

## 未验收项与恢复步骤

Windows 10、Windows Server 各版本、解锁后的正常交互桌面和浏览器实际显示尚未测试。现场先确认操作系统、build、普通输入桌面及授权环境，验证两个后端正常启动、可见页面交互、停止/断线/Agent 退出回收，再仅对实际通过的 build 范围标记 `passed`。不能用本次 headless 软件结果扩大平台支持范围。

HTTPS 静态站部署尚需发布者提供地址并配置证书；清单发布者仍是部署前替换占位。两源协议已通过本地动态 TLS 夹具验证，未声称已部署外网资源站。首版不强制发布者签名，来源信任边界见协议文档。
