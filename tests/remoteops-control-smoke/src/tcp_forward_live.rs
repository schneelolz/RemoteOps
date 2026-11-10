//! 真 Relay、Agent 和 MCP 的隔离 TCP 验收，固定目标仅使用本机测试服务。
use super::{
    AgentConfig, AgentEvent, AgentPermissionControl, Context, ControlSource, ControllerControlMode,
    Duration, Mcp, Path, Result, Value, agent_state, call, ensure, env, json, mpsc, pairing_code,
    raw_call, run_agent_with_permission_control, sleep, start_mcp, stop_agent, timeout, watch,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn admin(method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
    let address = env::var("CONTROL_SMOKE_ADMIN_ADDRESS")?;
    ensure!(
        address.parse::<std::net::SocketAddr>()?.ip().is_loopback(),
        "策略测试只能使用本机 Relay"
    );
    let token = env::var("REMOTEOPS_ADMIN_TOKEN")?;
    let mut request = reqwest::Client::new()
        .request(method, format!("http://{address}/api/admin/{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    ensure!(
        response.status().is_success(),
        "隔离管理 API 验收失败，HTTP {}",
        response.status()
    );
    Ok(response.json().await?)
}

async fn allow_target(agent: &Value, port: u16) -> Result<Value> {
    let policy = admin(reqwest::Method::GET, "tcp-forward-policy", None).await?;
    let revision = policy["revision"].as_u64().context("缺少策略修订")?;
    admin(reqwest::Method::PUT,"tcp-forward-policy",Some(json!({"expected_revision":revision,"policy":{
        "revision":revision,"rules":[{"id":"isolated-web","host":"127.0.0.1","port":port,"agent_id":agent,"installation_id":null,"purpose":"隔离 TCP 验收","expires_at":null}],
        "approval_contact":"本地隔离测试管理员","updated_at":null
    }}))).await
}

async fn clear_policy() -> Result<()> {
    let policy = admin(reqwest::Method::GET, "tcp-forward-policy", None).await?;
    admin(reqwest::Method::PUT,"tcp-forward-policy",Some(json!({"expected_revision":policy["revision"],"policy":{
        "revision":policy["revision"],"rules":[],"approval_contact":"本地隔离测试管理员","updated_at":null
    }}))).await?;
    Ok(())
}

async fn wait_listener_closed(address: &str) -> Result<()> {
    timeout(Duration::from_secs(5), async {
        loop {
            if TcpStream::connect(address).await.is_err() {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("监听端口未随撤销关闭")
}

async fn hold_echo_connection(address: &str) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(address).await?;
    stream.write_all(b"q").await?;
    let mut response = [0; 1];
    timeout(Duration::from_secs(5), stream.read_exact(&mut response)).await??;
    ensure!(response == *b"q", "配额连接未完成真实目标交换");
    Ok(stream)
}

async fn connection_is_rejected(address: &str) -> Result<()> {
    let mut stream = TcpStream::connect(address).await?;
    let mut response = [0; 1];
    let received = timeout(Duration::from_secs(5), stream.read(&mut response)).await?;
    ensure!(!matches!(received, Ok(1..)), "超限连接仍收到目标数据");
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn exercise_abrupt_connections(client: &Mcp, session: &str, agent: &Value) -> Result<()> {
    let target = TcpListener::bind("127.0.0.1:0").await?;
    let port = target.local_addr()?.port();
    allow_target(agent, port).await?;
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            tokio::spawn(async move {
                let mut byte = [0; 1];
                if stream.read_exact(&mut byte).await.is_err() {
                    return;
                }
                if byte == *b"x" {
                    // 浏览器取消下载时，目标仍有数据和确认在途。
                    let _ = stream.write_all(&vec![7; 512 * 1024]).await;
                    let _ = stream.shutdown().await;
                } else {
                    loop {
                        if stream.write_all(&byte).await.is_err()
                            || stream.read_exact(&mut byte).await.is_err()
                        {
                            break;
                        }
                    }
                }
            });
        }
    });
    let result = async {
        let forward = call(
            client,
            "create_tcp_forward",
            json!({"session_id":session,"host":"127.0.0.1","port":port}),
        )
        .await?;
        let address = forward["local_address"]
            .as_str()
            .context("缺少中断回归监听地址")?;
        let mut retained = hold_echo_connection(address).await?;
        for _ in 0..20 {
            for _ in 0..4 {
                let mut stream = TcpStream::connect(address).await?;
                stream.write_all(b"x").await?;
                let mut first_byte = [0; 1];
                timeout(Duration::from_secs(5), stream.read_exact(&mut first_byte)).await??;
                // 保留未读目标数据，关闭本地socket产生取消/复位时序。
                drop(stream);
            }
            retained.write_all(b"v").await?;
            let mut byte = [0; 1];
            timeout(Duration::from_secs(5), retained.read_exact(&mut byte)).await??;
            ensure!(byte == *b"v", "其他连接取消导致存活连接被关闭");
            let mut next = hold_echo_connection(address).await?;
            next.shutdown().await?;
            let mut rest = Vec::new();
            timeout(Duration::from_secs(5), next.read_to_end(&mut rest)).await??;
        }
        let status = call(client, "list_tcp_forwards", json!({})).await?;
        ensure!(
            status["forwards"]
                .as_array()
                .context("缺少连接状态")?
                .iter()
                .any(|f| f["forward_id"] == forward["forward_id"] && f["status"] == "listening"),
            "取消回归后监听已关闭"
        );
        clear_policy().await?;
        wait_listener_closed(address).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    server.abort();
    let _ = server.await;
    result
}

#[allow(clippy::too_many_lines)]
async fn exercise_connection_quotas(client: &Mcp, session: &str, agent: &Value) -> Result<()> {
    let target = TcpListener::bind("127.0.0.1:0").await?;
    let port = target.local_addr()?.port();
    allow_target(agent, port).await?;
    let (stop, _) = watch::channel(false);
    let stopped = stop.subscribe();
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            let mut stopped = stopped.clone();
            tokio::spawn(async move {
                let mut data = [0; 1024];
                loop {
                    tokio::select! {
                        _ = stopped.changed() => break,
                        result = stream.read(&mut data) => match result {
                            Ok(0) | Err(_) => break,
                            Ok(size) => if stream.write_all(&data[..size]).await.is_err() { break; },
                        }
                    }
                }
            });
        }
    });
    let result = async {
        let mut forwards = Vec::new();
        for _ in 0..3 {
            forwards.push(
                call(
                    client,
                    "create_tcp_forward",
                    json!({"session_id":session,"host":"127.0.0.1","port":port}),
                )
                .await?,
            );
        }
        let addresses = forwards
            .iter()
            .map(|f| f["local_address"].as_str().context("缺少配额监听地址"))
            .collect::<Result<Vec<_>>>()?;
        let mut held = Vec::new();
        for _ in 0..16 {
            held.push(hold_echo_connection(addresses[0]).await?);
        }
        connection_is_rejected(addresses[0]).await?;
        // 单条达到上限后，其他监听仍可用，直到进程总数达到32。
        for _ in 0..16 {
            held.push(hold_echo_connection(addresses[1]).await?);
        }
        connection_is_rejected(addresses[2]).await?;
        for stream in &mut held {
            stream.write_all(b"v").await?;
            let mut response = [0; 1];
            timeout(Duration::from_secs(5), stream.read_exact(&mut response)).await??;
            ensure!(response == *b"v", "拒绝超限连接影响了已有连接");
        }
        let mut released = held.pop().context("缺少可回收连接")?;
        released.shutdown().await?;
        let mut rest = Vec::new();
        timeout(Duration::from_secs(5), released.read_to_end(&mut rest)).await??;
        timeout(Duration::from_secs(5), async {
            loop {
                let status = call(client, "list_tcp_forwards", json!({})).await?;
                let count = status["forwards"]
                    .as_array()
                    .context("缺少转发状态")?
                    .iter()
                    .map(|f| f["active_connections"].as_u64().unwrap_or(0))
                    .sum::<u64>();
                if count == 31 {
                    break Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        held.push(hold_echo_connection(addresses[2]).await?);
        clear_policy().await?;
        for address in &addresses {
            wait_listener_closed(address).await?;
        }
        for stream in &mut held {
            let mut rest = Vec::new();
            let received = timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await?;
            ensure!(
                received.is_err() || rest.is_empty(),
                "撤权后连接仍收到新数据"
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = stop.send(true);
    server.abort();
    let _ = server.await;
    result
}

#[allow(clippy::too_many_lines)]
async fn exercise(
    client: &Mcp,
    events: &mpsc::Receiver<AgentEvent>,
    control: &AgentPermissionControl,
) -> Result<()> {
    let code = pairing_code(events).await?;
    let paired = call(client, "pair_connection", json!({"pairing_code":code})).await?;
    ensure!(
        paired["control_mode"] == "full_access"
            && paired["control_source"] == "local_default"
            && paired["control_sync_status"] == "synced",
        "配对响应没有等待现场默认授权初始化"
    );
    ensure!(
        paired["control_message"]
            .as_str()
            .is_some_and(|text| text.contains("现场默认")),
        "配对结果缺少默认授权告知"
    );
    let session = paired["session_id"].as_str().context("缺少测试会话")?;
    let info = call(client, "get_remoteops_info", json!({})).await?;
    ensure!(info["tcp_forward_version"] == 2, "Relay TCP 扩展未协商");
    let missing = raw_call(
        client,
        "run_readonly_command",
        json!({"session_id":session,"command":"hostname"}),
    )
    .await?;
    ensure!(missing.is_error == Some(true), "缺少 shell 未被契约拒绝");
    let read=call(client,"run_readonly_command",json!({"session_id":session,"command":"hostname","shell":if cfg!(windows){"cmd"}else{"system"}})).await?;
    ensure!(read["exit_code"] == 0, "显式 shell 只读查询失败");
    let guide = call(client, "refresh_global_guide", json!({})).await?;
    ensure!(
        guide["status"] == "empty"
            && guide["error_code"].is_null()
            && guide["message"].as_str().is_some_and(|v| !v.is_empty()),
        "空指引状态错误"
    );
    let policy = call(
        client,
        "get_tcp_forward_policy",
        json!({"session_id":session}),
    )
    .await?;
    ensure!(policy["policy"]["hot_reload"] == true, "缺少热更新说明");
    let target = TcpListener::bind("127.0.0.1:0").await?;
    let port = target.local_addr()?.port();
    let denied = raw_call(
        client,
        "create_tcp_forward",
        json!({"session_id":session,"host":"127.0.0.1","port":port}),
    )
    .await?;
    ensure!(denied.is_error == Some(true), "空策略没有拒绝目标");
    allow_target(&paired["agent_instance_id"], port).await?;
    let request = vec![0x31; 2 * 1024 * 1024];
    let response = vec![0x67; 3 * 1024 * 1024];
    let expected = request.clone();
    let sent = response.clone();
    let server = tokio::spawn(async move {
        let mut tasks = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = target.accept().await?;
            let expected = expected.clone();
            let sent = sent.clone();
            tasks.push(tokio::spawn(async move {
                sleep(Duration::from_millis(100)).await;
                let mut actual = Vec::new();
                stream.read_to_end(&mut actual).await?;
                ensure!(actual == expected, "转发请求内容不一致");
                stream.write_all(&sent).await?;
                stream.shutdown().await?;
                Ok::<_, anyhow::Error>(())
            }));
        }
        for task in tasks {
            task.await??;
        }
        Ok::<_, anyhow::Error>(())
    });
    let forward = call(
        client,
        "create_tcp_forward",
        json!({"session_id":session,"host":"127.0.0.1","port":port}),
    )
    .await?;
    let address = forward["local_address"].as_str().context("缺少本地监听")?;
    let exchange = |address: String, request: Vec<u8>, response: Vec<u8>| async move {
        let mut stream = TcpStream::connect(&address).await?;
        stream.write_all(&request).await?;
        stream.shutdown().await?;
        let mut actual = Vec::new();
        stream.read_to_end(&mut actual).await?;
        ensure!(actual == response, "转发响应内容不一致");
        Ok::<_, anyhow::Error>(())
    };
    let (first, second) = tokio::join!(
        exchange(address.into(), request.clone(), response.clone()),
        exchange(address.into(), request.clone(), response.clone())
    );
    first?;
    second?;
    exchange(address.into(), request, response).await?;
    server.await??;
    let status = call(client, "list_tcp_forwards", json!({})).await?;
    ensure!(
        status["forwards"]
            .as_array()
            .is_some_and(|forwards| forwards.iter().any(|entry| entry["forward_id"]
                == forward["forward_id"]
                && entry["transferred_bytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes >= 15 * 1024 * 1024))),
        "TCP 累计流量或状态不正确"
    );
    clear_policy().await?;
    wait_listener_closed(address).await?;
    exercise_abrupt_connections(client, session, &paired["agent_instance_id"]).await?;
    exercise_connection_quotas(client, session, &paired["agent_instance_id"]).await?;
    let audit = admin(reqwest::Method::GET, "audit?limit=100", None).await?;
    ensure!(
        audit.to_string().contains("control_mode_changed"),
        "权限变更缺少管理员审计事件"
    );
    // 重新批准后，现场撤权必须关闭空闲监听。
    let target = TcpListener::bind("127.0.0.1:0").await?;
    let port = target.local_addr()?.port();
    allow_target(&paired["agent_instance_id"], port).await?;
    let forward = call(
        client,
        "create_tcp_forward",
        json!({"session_id":session,"host":"127.0.0.1","port":port}),
    )
    .await?;
    let snapshot = agent_state(
        events,
        ControllerControlMode::FullAccess,
        Some(ControlSource::LocalDefault),
    )
    .await?;
    control.request_control(snapshot, ControllerControlMode::StepByStep)?;
    wait_listener_closed(forward["local_address"].as_str().context("缺少监听地址")?).await?;
    clear_policy().await?;
    println!(
        "通过：真实三端 TCP 策略热更新、大数据与慢读、半关闭、80次取消下载不影响已有连接、16/32配额与回收、策略和现场撤权清理"
    );
    Ok(())
}

pub(super) async fn run(root: &Path, address: &str, cert: &Path) -> Result<()> {
    let config = AgentConfig {
        default_full_control: true,
        retry_seconds: 1,
        relay: address.into(),
        server_name: "localhost".into(),
        ca_cert: Some(cert.to_owned()),
        transfer_root: root.join("transfers"),
        state_file: root.join("agent.json"),
        ..Default::default()
    };
    let control = AgentPermissionControl::from_config(&config);
    let (sender, events) = mpsc::channel();
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run_agent_with_permission_control(
        config,
        Some(sender),
        stopped,
        control.clone(),
    ));
    let mut agent = Some(task);
    let client = start_mcp(root, address, cert).await?;
    let result = timeout(
        Duration::from_secs(90),
        exercise(&client, &events, &control),
    )
    .await
    .context("TCP 三端专项验收超时")
    .and_then(|r| r);
    let _ = client.cancel().await;
    let cleanup = stop_agent(&stop, &mut agent).await;
    result?;
    cleanup?;
    Ok(())
}
