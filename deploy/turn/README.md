# RemoteOps 的可选 MiniRTC 中继

RemoteOps Relay 负责配对、审批、人工接管、键鼠、断开和媒体协商。coturn 独立运行，可以与 Relay 共用一台主机和公网地址。运维端在「画质 → 传输方式」中选择「MiniRTC · TURN/UDP」，默认「自动」沿用局域网直连和 TCP 中继。当前 MiniRTC 组件支持 Windows x64。

## 端口和配置

| 服务 | 入站 | 用途 |
| --- | --- | --- |
| RemoteOps Relay | 7443/TCP | TLS 控制和原有媒体链路 |
| coturn | 7443/UDP | 客户端接入 TURN |
| coturn | 25000–25063/UDP | 按分配转发媒体 |

将 `turnserver.conf.template` 渲染为同目录的 `turnserver.conf`。公网 NAT 部署要分别填写公网映射地址和主机网卡地址。REST 共享密钥使用 32 字节随机数的 64 位十六进制编码，配置文件应只对管理员和容器服务用户可读。用 `docker compose -p remoteops-turn up -d` 启动独立服务。

在 Relay 状态文件旁放置 `turn-issuer.json`，字段为公网 IPv4 `address`、数字 `port` 和与 coturn 相同的 `secret`。也可显式指定 `--turn-config`。Relay 在启动时校验配置，已配对的人工桌面两端各获得一份限时凭据。客户端只在验证对端身份指纹并收到 MiniRTC 关键帧后切换画面。旧端、缺少组件或 UDP 协商失败时使用原有媒体路径。

## 已发布版本

发布方在 Relay 状态文件旁维护 `release-catalog.json`，也可指定 `--release-catalog`。文件在 Relay 启动时加载，客户端在连接握手时读取。按实际交付的产品、OS、架构登记 SemVer 版本，同一组合只能有一条记录。

```json
{
  "schema_version": 1,
  "releases": [
    { "product": "jlai-desktop-online", "os": "windows", "arch": "x86_64", "version": "0.4.0-jlai.39" },
    { "product": "jlai-human-desktop", "os": "windows", "arch": "x86_64", "version": "0.1.0-preview.15" },
    { "product": "jlai-mcp", "os": "windows", "arch": "x86_64", "version": "0.2.0-preview.35" }
  ]
}
```

## 构建与测量

`scripts/Build-MiniRtcWorker.ps1` 从固定版本的 MiniRTC 构建独立媒体程序。构建两个 Windows 端时，使用 `REMOTEOPS_MINIRTC_SHA256` 绑定最终交付的媒体程序摘要，将 `remoteops-minirtc.exe` 放在对应主程序旁。定制版构建脚本的 `-MiniRtcWorker` 参数会完成这两步。如需签名，应先给媒体程序签名，再构建两端。

`RemoteOps.Desktop.Smoke --relay-latency <结果.json>` 在专用 Windows 测试桌面上运行。该模式从标准输入读取同机测试 Agent 的临时配对码，使用真实人工身份、媒体组件和原生解码器。按 TCP、MiniRTC、MiniRTC、TCP 四组发送 F23，计量从运维端键盘事件到该窗口在 Windows 合成桌面上的色块变化。只有指定路由和控制模式生效才记样本。每组首样本预热，原始结果保存各次毫秒数。运行时运维窗口和色块须可见。
