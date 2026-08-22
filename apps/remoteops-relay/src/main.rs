mod relay;

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use chrono::Duration;
use clap::Parser;
use remoteops_domain::ControllerOwnerId;
use remoteops_protocol::load_server_config;
use tokio::{net::TcpListener, sync::Semaphore, time};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::relay::Relay;

/// `RemoteOps` TLS 中继服务。
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// TLS 业务监听地址。
    #[arg(long, env = "REMOTEOPS_RELAY_BIND", default_value = "0.0.0.0:7443")]
    bind: String,
    /// Docker 健康检查监听地址。
    #[arg(long, env = "REMOTEOPS_HEALTH_BIND", default_value = "0.0.0.0:8080")]
    health_bind: String,
    /// PEM 服务端证书。
    #[arg(long, env = "REMOTEOPS_TLS_CERT", default_value = "/data/tls/cert.pem")]
    tls_cert: PathBuf,
    /// PEM 服务端私钥。
    #[arg(long, env = "REMOTEOPS_TLS_KEY", default_value = "/data/tls/key.pem")]
    tls_key: PathBuf,
    /// Relay 重启恢复所使用的状态文件。
    #[arg(
        long,
        env = "REMOTEOPS_STATE_FILE",
        default_value = "/data/relay-state.json"
    )]
    state_file: PathBuf,
    /// 首次启动生成自签名证书时写入的 SAN；逗号分隔。
    #[arg(
        long,
        env = "REMOTEOPS_TLS_SANS",
        value_delimiter = ',',
        default_value = "localhost,127.0.0.1,remoteops-relay"
    )]
    tls_sans: Vec<String>,
    /// Agent 心跳续租后的租约分钟数。
    #[arg(long, env = "REMOTEOPS_LEASE_MINUTES", default_value_t = 10)]
    lease_minutes: i64,
    /// 测试环境可用的秒级租约；设置后优先于 lease-minutes。
    #[arg(long, env = "REMOTEOPS_LEASE_SECONDS")]
    lease_seconds: Option<u64>,
    /// Agent 建议心跳间隔秒数。
    #[arg(long, env = "REMOTEOPS_HEARTBEAT_SECONDS", default_value_t = 15)]
    heartbeat_seconds: u64,
    /// 同时处理的 TLS 业务连接上限。
    #[arg(long, env = "REMOTEOPS_MAX_CONNECTIONS", default_value_t = 1024)]
    max_connections: usize,
    /// TLS 握手超时秒数。
    #[arg(
        long,
        env = "REMOTEOPS_TLS_HANDSHAKE_TIMEOUT_SECONDS",
        default_value_t = 10
    )]
    tls_handshake_timeout_seconds: u64,
    /// 人工 Controller 使用的独立认证令牌。
    #[arg(long, env = "REMOTEOPS_HUMAN_CONTROLLER_TOKEN", hide_env_values = true)]
    human_controller_token: String,
    /// AI Controller 使用的独立认证令牌。
    #[arg(long, env = "REMOTEOPS_AI_CONTROLLER_TOKEN", hide_env_values = true)]
    ai_controller_token: String,
    /// 此 Relay 部署允许接入的唯一 Controller Owner。
    #[arg(long, env = "REMOTEOPS_CONTROLLER_OWNER_ID")]
    controller_owner_id: ControllerOwnerId,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    validate_controller_tokens(&args.human_controller_token, &args.ai_controller_token)?;
    if args.controller_owner_id.as_uuid().is_nil() {
        anyhow::bail!("Controller Owner ID 不能使用全零 UUID");
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("remoteops_relay=info")),
        )
        .with_target(false)
        .compact()
        .init();

    ensure_certificate(&args.tls_cert, &args.tls_key, &args.tls_sans)?;
    let tls = load_server_config(&args.tls_cert, &args.tls_key).with_context(|| {
        format!(
            "无法加载 Relay TLS 证书 {} 和私钥 {}",
            args.tls_cert.display(),
            args.tls_key.display()
        )
    })?;
    let listener = TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("无法监听 {}", args.bind))?;
    let health_listener = TcpListener::bind(&args.health_bind)
        .await
        .with_context(|| format!("无法监听健康检查 {}", args.health_bind))?;

    let lease_lifetime = args.lease_seconds.map_or_else(
        || Duration::minutes(args.lease_minutes.max(1)),
        |seconds| Duration::seconds(i64::try_from(seconds.max(1)).unwrap_or(i64::MAX)),
    );
    let relay = Arc::new(
        Relay::new_with_state_path(
            lease_lifetime,
            args.heartbeat_seconds,
            args.controller_owner_id,
            args.human_controller_token,
            args.ai_controller_token,
            &args.state_file,
        )
        .with_context(|| {
            format!(
                "无法加载或初始化 Relay 状态文件 {}",
                args.state_file.display()
            )
        })?,
    );
    tokio::spawn(health_server(health_listener));
    relay.clone().spawn_cleanup();
    let connection_slots = Arc::new(Semaphore::new(args.max_connections.max(1)));
    let tls_handshake_timeout =
        time::Duration::from_secs(args.tls_handshake_timeout_seconds.max(1));

    info!(
        bind = %args.bind,
        health_bind = %args.health_bind,
        state_file = %args.state_file.display(),
        "RemoteOps Relay 已启动"
    );
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer) = result?;
                let relay = relay.clone();
                let tls = tls.clone();
                let Ok(connection_slot) = connection_slots.clone().try_acquire_owned() else {
                    warn!(%peer, "Relay 连接数已达到上限，拒绝新连接");
                    continue;
                };
                tokio::spawn(async move {
                    let _connection_slot = connection_slot;
                    match time::timeout(
                        tls_handshake_timeout,
                        remoteops_protocol::accept_tls(stream, &tls),
                    )
                    .await
                    {
                        Ok(Ok(stream)) => {
                            if let Err(error) = relay.handle_client(stream).await {
                                error!(%peer, error = %error, "客户端连接异常结束");
                            }
                        }
                        Ok(Err(error)) => {
                            error!(%peer, error = %error, "TLS 握手失败");
                        }
                        Err(_) => warn!(%peer, "TLS 握手超时"),
                    }
                });
            }
            () = &mut shutdown => {
                info!("RemoteOps Relay 收到停止信号");
                break;
            }
        }
    }
    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn validate_controller_tokens(human: &str, ai: &str) -> anyhow::Result<()> {
    const MINIMUM_TOKEN_BYTES: usize = 32;
    if human.len() < MINIMUM_TOKEN_BYTES || ai.len() < MINIMUM_TOKEN_BYTES {
        anyhow::bail!("Controller 认证令牌必须至少包含 {MINIMUM_TOKEN_BYTES} 个字节");
    }
    if human == ai {
        anyhow::bail!("人工和 AI Controller 必须配置不同的认证令牌");
    }
    Ok(())
}

fn ensure_certificate(
    certificate_path: &std::path::Path,
    private_key_path: &std::path::Path,
    subject_alt_names: &[String],
) -> anyhow::Result<()> {
    match (certificate_path.exists(), private_key_path.exists()) {
        (true, true) => return Ok(()),
        (true, false) | (false, true) => {
            anyhow::bail!("TLS 证书和私钥必须同时存在或同时缺失");
        }
        (false, false) => {}
    }
    let certificate_parent = certificate_path
        .parent()
        .context("TLS 证书路径缺少父目录")?;
    let key_parent = private_key_path
        .parent()
        .context("TLS 私钥路径缺少父目录")?;
    std::fs::create_dir_all(certificate_parent)?;
    std::fs::create_dir_all(key_parent)?;
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(subject_alt_names.to_vec())
            .context("生成 Relay 自签名证书失败")?;
    std::fs::write(certificate_path, cert.pem())?;
    std::fs::write(private_key_path, signing_key.serialize_pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(private_key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    info!(
        certificate = %certificate_path.display(),
        "已生成 Relay 自签名证书；首次配发 Agent 时必须复制并固定信任该证书"
    );
    Ok(())
}

async fn health_server(listener: TcpListener) {
    loop {
        let Ok((mut stream, _peer)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let response =
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::validate_controller_tokens;

    #[test]
    fn controller_tokens_must_be_long_and_independent() {
        assert!(validate_controller_tokens("short", "also-short").is_err());
        let shared = "shared-controller-token-at-least-32-bytes";
        assert!(validate_controller_tokens(shared, shared).is_err());
        assert!(
            validate_controller_tokens(
                "human-controller-token-at-least-32-bytes",
                "ai-controller-token-value-at-least-32-bytes",
            )
            .is_ok()
        );
    }
}
