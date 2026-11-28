# 独立管理前端部署

管理前端从 Relay `0.2.0-preview.17` 起独立运行。前端版本 `0.1.0`，源码和开发说明见 [relay-admin](../../relay-admin/README.md)。

首次源码构建先在仓库根目录生成静态产物，再构建 Compose 中的两个镜像：

```powershell
npm --prefix relay-admin ci --ignore-scripts
npm --prefix relay-admin run build
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml config --quiet
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml up -d --build
```

已有生产环境升级前必须备份完整状态、数据库、Compose 和环境文件，核对镜像版本与摘要。不得通过上述首次部署示例跳过备份。已验证的当前批次上线与回退记录见 [preview.17 交付](../../docs/relay-admin-split-preview17.md)。

管理入口默认回环 `18081` 转发到前端 `8080`，后端 API 默认回环 `18082`。前端通过 Compose 网络访问 `remoteops-relay:18081`。两者继续在浏览器同源访问，保持现有 HTTPS 反向代理目标端口即可。

只更新页面时生成新的前端版本和镜像，修改 `REMOTEOPS_RELAY_ADMIN_IMAGE` 后只更新前端服务：

```powershell
docker compose --env-file deploy/relay/.env -f deploy/relay/docker-compose.yml up -d --no-build --no-deps remoteops-relay-admin
```

只更新后端时修改 `REMOTEOPS_RELAY_IMAGE`，仅更新 `remoteops-relay`。Nginx 使用 Docker DNS 动态解析后端地址，不要求一起重建前端。始终先确认前端最低 API 版本兼容，不同内容不得覆盖同版本。
