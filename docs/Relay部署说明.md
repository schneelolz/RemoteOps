# Relay 部署说明

本文面向第一次部署 RemoteOps Relay 的部署者，适用于公开源码版本 `0.2.0-preview.1`。

Relay 是现场 Agent 与工程师本机 MCP 之间的中转服务。首版官方部署路径是 Linux x64 + Docker；Windows 宿主机通过 Docker Desktop 或 WSL2 运行 Linux 容器可以作为试点方式，但当前没有独立平台验收。Relay 核心程序虽然可以在 Windows 编译，原生 Windows 服务化部署暂不属于首版交付路径。

## 一、部署前准备

### 主机和网络

- Linux x64 主机；
- Docker Engine 和 Docker Compose 插件；
- Agent 和工程师本机都能访问的域名或固定 IP；
- 对外可访问的 TCP `7443`；
- 写入 TLS 证书 SAN 的实际域名或 IP。

健康检查端口默认是 `18080`，只绑定 Relay 主机回环地址，不应暴露到公网。如果 Relay 位于云防火墙或反向代理之后，需要放行 Agent/Controller 到 Relay 的 `7443`，不要开放无关端口。

### 凭据和证书

部署需要：

1. Human Controller Token；
2. AI Controller Token；
3. Human 与 AI 共用的非全零 Controller Owner UUID。

两个 Controller Token 必须互不相同，且长度至少 32 字节。Agent 首次连接不需要入网码或部署级注册 Token。Token、Owner UUID、TLS 私钥和控制码不得写入 Git、聊天、截图或普通日志。

正式部署应使用受信任 CA 证书，证书 SAN 必须包含 Agent 和 Controller 实际连接时使用的域名或 IP。首次实验如果没有证书，Relay 会在 Docker 数据卷中自动生成自签名证书；这种方式只适合封闭实验，Agent 和 MCP 必须通过 PEM 或人工核对的 SHA-256 指纹显式信任它。

## 二、获取源码并准备配置

在部署主机上进入 RemoteOps 仓库根目录。正式发布后建议固定到具体版本或 Release 标签：

```bash
export REMOTEOPS_REPOSITORY_URL='<从 GitHub 仓库页面复制的 HTTPS 或 SSH clone URL>'
git clone "$REMOTEOPS_REPOSITORY_URL"
cd RemoteOps
git checkout <版本标签>
```

复制部署模板：

```bash
cp deploy/relay/.env.example deploy/relay/.env
```

编辑 `deploy/relay/.env`，至少确认以下值：

```dotenv
REMOTEOPS_TLS_SANS=relay.example.com,remoteops-relay,localhost,127.0.0.1
REMOTEOPS_RELAY_PORT=7443
REMOTEOPS_HEALTH_PORT=18080
REMOTEOPS_RELAY_ADMIN_PORT=18081
REMOTEOPS_ADMIN_TOKEN=<至少 32 字节的随机管理 Token>
REMOTEOPS_ADMIN_USERNAME=<管理页面用户名>
REMOTEOPS_ADMIN_PASSWORD=<至少 16 字节的随机管理密码>
REMOTEOPS_ADMIN_PASSWORD_FORCE_RESET=false
REMOTEOPS_HUMAN_CONTROLLER_TOKEN=<至少 32 字节的随机值>
REMOTEOPS_AI_CONTROLLER_TOKEN=<另一组至少 32 字节的随机值>
REMOTEOPS_CONTROLLER_OWNER_ID=<非全零 UUID>
```

`REMOTEOPS_TLS_SANS` 必须包含 Agent 和 Controller 实际访问的域名或 IP。示例中的 `relay.example.com` 是占位符，不能原样使用。

收紧配置文件权限，然后静默检查 Compose 配置。`config --quiet` 不会打印解析后的 Token：

```bash
chmod 600 deploy/relay/.env
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml config --quiet
```

## 三、启动和验证

启动 Relay。默认会从当前源码构建镜像，并把身份状态保存到独立 Docker Volume：

```bash
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml up -d --build
```

检查容器状态、健康接口和最近日志：

```bash
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml ps
curl --fail http://127.0.0.1:18080/health
curl --fail -H "Authorization: Bearer $REMOTEOPS_ADMIN_TOKEN" http://127.0.0.1:18081/api/admin/overview
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml logs --tail 100
```

管理页面位于 `http://127.0.0.1:18081/`；通过 HTTPS 反向代理访问时会自动使用当前域名。页面登录使用管理用户名和密码，服务端发放 HttpOnly Cookie Session；Bearer Token 仅保留给内部脚本和应急 API。管理服务默认只监听宿主机回环地址；如果需要远程访问，请放在 HTTPS 反向代理后面。管理页面不会返回任何 Controller Token、Agent resume token 或 Session binding token 明文。

登录后进入“安全设置”即可修改管理页面密码。密码会以随机盐哈希形式保存到 Relay 状态文件，重启后仍然有效，修改后所有已有登录 Session 会失效。忘记密码时，在 `.env` 中设置新的 `REMOTEOPS_ADMIN_PASSWORD`，临时将 `REMOTEOPS_ADMIN_PASSWORD_FORCE_RESET=true`，重新部署一次后务必恢复为 `false`。不要删除状态数据卷来重置密码，否则会同时丢失 Agent 恢复状态和审计日志。

健康接口返回 `ok`，且容器状态为 `healthy` 后，使用下面的形式记录 Relay 地址：

```text
relay.example.com:7443
```

Agent 的 Relay 地址、Agent 的 TLS 服务名和 MCP 的 Relay 地址必须指向同一个实际入口。证书 SAN、DNS、端口或反向代理任一项不匹配，都会导致 TLS 连接失败。

## 四、接入 Agent 和 MCP

Relay 部署完成后：

1. 在工程师本机安装 Windows x64 MCP，配置相同的 Relay 地址和 Owner UUID；
2. 在现场 Windows 电脑运行 `remoteops-agent-gui.exe`，首次只填写 Relay 地址并读取临时控制码；
3. 先完成一次只读配对和目标核对；
4. 修改操作由当前 Codex 逐项确认，或由用户在当前 Codex 中为精确 `session_id` 开启临时完全控制；Agent 端没有授权按钮。

公网 CA 证书不需要向 Agent 分发文件。Relay 自动生成自签名证书时，Agent GUI 会在发送 RemoteOps 凭据前显示叶证书 SHA-256 指纹；部署管理员应通过独立可信渠道提供正确指纹，由现场用户核对后保存。CLI、Service 和 MCP 没有证书确认界面，必须预置 CA PEM 或已核对指纹。

详细步骤见：

- [现场被控端 GUI 使用说明](现场被控端GUI使用说明.md)
- [RemoteOps MCP 使用手册](RemoteOpsMCP使用手册.md)
- [Codex MCP 接入说明](CodexMCP接入说明.md)

## 五、停止、更新和数据保护

停止容器但保留 Relay 状态卷：

```bash
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml down
```

重新构建并启动：

```bash
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml up -d --build
```

不要随意执行 `docker compose down -v` 或删除 `remoteops-relay-data`，否则会丢失用于恢复 Agent 身份、控制码和逻辑会话的状态数据。

Relay 状态保存在 `/data/relay-state.json`，TLS 文件保存在 `/data/tls`。正式环境应保护对应 Docker Volume，并在升级或迁移前制作可恢复备份。

## 六、常见问题

### 7443 端口无法连接

确认容器状态、主机防火墙、云安全组、反向代理和 DNS。健康检查端口 `18080` 默认只允许在 Relay 主机本机访问，不能用公网访问它判断外部链路是否可用。

### TLS 证书错误

确认 Relay 地址中的域名或 IP 出现在证书 SAN 中，并且 Agent/MCP 使用的 TLS 服务名与证书一致。自签名证书必须显式提供 CA PEM 或通过独立渠道核对指纹。

### Token 或 Owner 错误

Human 与 AI Controller Token 必须互不相同，且使用同一个 Owner UUID。Token 长度不足、Owner 使用全零 UUID 或 MCP 使用了另一组 Owner，都会导致认证或配对失败。Agent 端不读取 Controller Token，也不需要 Agent 注册 Token。

### 容器反复重启

```bash
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml logs --tail 200
```

重点检查 `.env` 是否缺少必填值、TLS SAN 是否为空、状态卷是否可写，以及主机端口是否已被占用。

## 七、安全边界

- Agent 只主动连接 Relay，不要求现场机器开放公网入站端口；
- 公网只暴露 TLS 业务端口，不暴露健康检查端口；
- Relay 不替代防火墙、堡垒机、终端防护或资产管理系统；
- 发布前必须独立复核 Token、TLS 私钥、Owner UUID、控制码和状态卷备份策略。

更完整的安全说明见 [安全模型](安全模型.md) 和 [发布与产物说明](发布与产物说明.md)。
