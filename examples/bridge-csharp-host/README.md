# C# 宿主示例

本示例兼容 .NET Framework 4.6.2 与 .NET 10。C# 启动独立 Rust 进程，不加载 Rust DLL，不需要普通 Agent EXE。

```powershell
dotnet build BridgeHost.csproj
dotnet run --framework net10.0 -- C:\候选\RemoteOpsBridge.exe grclient initialize.example.json
```

先将模板复制到仓库外，填写自己 Relay 的端点及 CA 或指纹；CA 和指纹均为空时使用系统证书信任。未知证书不会自动接受。服务端角色改为 udserver，建议省略 dataRoot，使用 ProgramData；正式部署由安装程序预设仅服务账号与管理员可访问的目录 ACL。

标准输出仅 JSON-RPC 2.0，每行一条 UTF-8 消息；本机协议 1，Relay 协议 22。initialize 只表示配置已接受，bridge.status 的 ready 才表示取得授权码。status 可读取当前状态；shutdown 有序停止。host.invoke 是 Bridge 对宿主的反向业务请求。默认超时 30 秒、单条 256 KiB、并发 16；取消使用 $/cancelRequest 通知。异常或结果未知的调用不重放。日志走标准错误，凭据不放命令行。

宿主必须验证精确 instanceId、能力版本、方法白名单、参数和返回字段；演示只允许空参数、模拟用户/门店查询和本机 IP 查询。实例路径为 dataRoot/角色/实例/Relay端点摘要，身份、传输、缓存、临时文件、日志及诊断目录分别隔离。Bridge 默认无桌面且只读，完全控制不能扩大能力白名单。

这不是 GRClient/UDServer 的正式业务接入，也不依赖尚未批准的 JLAI.AI.CapabilityProtocol 包。
