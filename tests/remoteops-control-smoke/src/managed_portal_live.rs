//! 真实回环 HTTP 门户、TLS Agent 运行库与通用 MCP 的受管接入验收。
use super::{Mcp, call, pair_result_proxy, raw_call, start_mcp};
use anyhow::{Context, Result, ensure};
use remoteops_agent::{
    AgentConfig, AgentEvent, AgentPermissionControl, AgentRunMode, AgentRuntimeOptions,
    ProductProfile, run_agent_with_options,
};
use remoteops_domain::{RequestId, TargetGuidance};
use remoteops_protocol::PairingMode;
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{env, path::Path, sync::mpsc, time::Duration};
use tokio::{
    sync::watch,
    time::{sleep, timeout},
};

struct Portal {
    client: Client,
    base: String,
}
impl Portal {
    async fn request(&self, method: Method, path: &str, token: &str, body: Value) -> Result<Value> {
        let response = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "本地门户请求失败：{}",
            response.status()
        );
        Ok(response.json().await?)
    }
    async fn status(&self, token: &str) -> Result<Value> {
        self.request(
            Method::GET,
            "/api/portal/v1/assets/service-smoke/remote-access",
            token,
            json!({}),
        )
        .await
    }
    async fn issue(&self, token: &str) -> Result<Value> {
        self.request(
            Method::POST,
            "/api/portal/v1/assets/service-smoke/invitations",
            token,
            json!({"idempotency_key":RequestId::new().to_string()}),
        )
        .await
    }
}

async fn pair(client: &Mcp, code: &str) -> Result<String> {
    let response = call(client, "pair_connection", json!({"pairing_code":code})).await?;
    Ok(response["session_id"]
        .as_str()
        .context("配对结果缺少 session")?
        .to_owned())
}

#[allow(clippy::too_many_lines)]
pub(super) async fn run(root: &Path, address: &str, cert: &Path) -> Result<()> {
    let admin_address = env::var("CONTROL_SMOKE_ADMIN_ADDRESS").context("缺少隔离门户监听地址")?;
    let socket: std::net::SocketAddr = admin_address.parse()?;
    ensure!(socket.ip().is_loopback(), "门户验收仅允许本地回环");
    let portal = Portal {
        client: Client::builder().timeout(Duration::from_secs(10)).build()?,
        base: format!("http://{admin_address}"),
    };
    let admin = env::var("REMOTEOPS_ADMIN_TOKEN").context("缺少隔离管理凭据")?;
    let principal = portal.request(Method::POST, "/api/admin/portal/principals", &admin,
        json!({"name":"local-service-smoke","owner_id":env::var("REMOTEOPS_CONTROLLER_OWNER_ID")?,"allowed_assets":["service-smoke"]})).await?;
    let portal_token = principal["token"].as_str().context("缺少门户凭据")?;
    let asset = portal.request(Method::POST, "/api/admin/portal/assets", &admin,
        json!({"asset_id":"service-smoke","principal_id":principal["principal"]["principal_id"]})).await?;
    let enrollment_file = root.join("enrollment.json");
    std::fs::write(
        &enrollment_file,
        serde_json::to_vec(&json!({
            "target_id":asset["asset"]["target_id"],"token":asset["enrollment_token"]
        }))?,
    )?;
    let profile = ProductProfile {
        id: "managed-service-smoke".into(),
        ..ProductProfile::default()
    };
    let mut options = AgentRuntimeOptions::service(profile);
    // 本地验收使用真实前台宿主，不把它标记为 SCM 或 Session 0。
    options.run_mode = AgentRunMode::Console;
    options.pairing_mode = PairingMode::ManagedOnDemand;
    options.guidance = vec![TargetGuidance {
        id: "service-smoke-guide".into(),
        version: "1".into(),
        source: "built_in_product".into(),
        text: "SERVICE_RUNTIME_GUIDANCE".into(),
        references: Vec::new(),
    }];
    let config = AgentConfig {
        relay: address.to_owned(),
        server_name: "localhost".into(),
        ca_cert: Some(cert.to_owned()),
        retry_seconds: 1,
        transfer_root: root.join("transfers"),
        state_file: root.join("agent.json"),
        managed_enrollment_file: Some(enrollment_file),
        ..AgentConfig::default()
    };
    let (events, receiver) = mpsc::channel();
    let (stop, shutdown) = watch::channel(false);
    let agent = tokio::spawn(run_agent_with_options(
        config,
        Some(events),
        shutdown,
        AgentPermissionControl::default(),
        options,
    ));
    timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = receiver.try_recv() {
                ensure!(
                    !matches!(event, AgentEvent::Connected { .. }),
                    "受管待命不能产生自动码"
                );
                if matches!(event, AgentEvent::Registered { .. }) {
                    return Ok::<_, anyhow::Error>(());
                }
                ensure!(
                    !matches!(event, AgentEvent::Failed { .. }),
                    "受管真实运行库启动失败"
                );
            }
            sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("受管 Agent 注册超时")??;
    let status = portal.status(portal_token).await?;
    ensure!(
        status["online"] == true
            && status["invitation"].is_null()
            && status["connection"].is_null(),
        "待命状态不正确"
    );
    let proxy = pair_result_proxy::start(address, cert).await?;
    let client = start_mcp(root, &proxy.address, cert).await?;
    let issued = portal.issue(portal_token).await?;
    let code = issued["code"].as_str().context("门户未返回控制码")?;
    let session = pair(&client, code).await?;
    ensure!(
        proxy.dropped.load(std::sync::atomic::Ordering::SeqCst) == 1,
        "首次受管配对必须在结果丢失后由同一次调用恢复"
    );
    println!("通过：首次受管PairResult丢失并断开TLS后，同次MCP调用以原request_id恢复一次消费结果");
    let info = call(&client, "get_target_info", json!({"session_id":session})).await?;
    let encoded = serde_json::to_string(&info)?;
    ensure!(
        encoded.contains("managed-service-smoke")
            && encoded.contains("SERVICE_RUNTIME_GUIDANCE")
            && encoded.contains("product_policy_disabled"),
        "上下文/产品策略未通过通用 MCP 返回"
    );
    ensure!(
        !encoded.contains("enrollment_token") && !encoded.contains("connection_resume_token"),
        "目标信息不得泄露凭据"
    );
    let caps = call(
        &client,
        "desktop_capabilities",
        json!({"session_id":session}),
    )
    .await?;
    ensure!(
        serde_json::to_string(&caps)?.contains("product_policy_disabled"),
        "服务图形能力必须拒绝"
    );
    call(&client, "close_connection", json!({"session_id":session})).await?;
    ensure!(
        portal.status(portal_token).await?["connection"].is_null(),
        "MCP 关闭必须持久撤销门户授权"
    );
    let old = raw_call(&client, "pair_connection", json!({"pairing_code":code})).await?;
    ensure!(old.is_error == Some(true), "已关闭旧码不能重新配对");
    let issued = portal.issue(portal_token).await?;
    let code2 = issued["code"].as_str().context("第二次签码缺少 code")?;
    ensure!(code2 != code, "重签不能复用旧码");
    let session2 = pair(&client, code2).await?;
    ensure!(session2 != session, "新授权必须使用新逻辑会话");
    let active = portal.status(portal_token).await?;
    let connection = active["connection"]["connection_id"]
        .as_str()
        .context("缺少活动连接")?;
    portal
        .request(
            Method::POST,
            &format!("/api/portal/v1/connections/{connection}/close"),
            portal_token,
            json!({}),
        )
        .await?;
    ensure!(
        portal.status(portal_token).await?["connection"].is_null(),
        "门户关闭必须撤销活动连接"
    );
    let _ = stop.send(true);
    timeout(Duration::from_secs(10), agent)
        .await
        .context("Agent停止超时")???;
    println!(
        "通过：真实门户HTTP + TLS受管Agent运行库 + 通用MCP；无码待命、按需签码、上下文、禁图形、双向关闭与重签（前台宿主，非SCM）"
    );
    Ok(())
}
