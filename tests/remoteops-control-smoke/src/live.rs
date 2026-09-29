//! 真实 Agent、真实 MCP 与本地 TLS Relay 的授权生命周期验收。
use anyhow::{Context, Result, bail, ensure};
use remoteops_agent::{
    AgentConfig, AgentEvent, AgentPermissionControl, run_agent_with_permission_control,
};
use remoteops_domain::RequestId;
use remoteops_protocol::{ControlSource, ControllerControlMode, SessionControlState};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::RunningService,
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc,
    time::Duration,
};
use tokio::{
    process::Command,
    sync::watch,
    time::{sleep, timeout},
};

type Mcp = RunningService<rmcp::RoleClient, ()>;

// 仅清理本测试创建的独立临时目录。
struct Workspace(PathBuf);
impl Workspace {
    fn create() -> Result<Self> {
        let base = env::temp_dir().canonicalize().context("读取临时目录失败")?;
        let path = base.join(format!("remoteops-control-live-{}", RequestId::new()));
        ensure!(
            path.parent() == Some(base.as_path()),
            "临时目录必须位于系统临时根内"
        );
        std::fs::create_dir(&path).context("创建独立测试目录失败")?;
        Ok(Self(path))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            eprintln!(
                "测试临时目录清理未完成，请检查系统临时目录中的 remoteops-control-live 前缀目录"
            );
        }
    }
}

async fn start_mcp(root: &Path, address: &str, cert: &Path) -> Result<Mcp> {
    let executable = match env::var_os("CONTROL_SMOKE_MCP_EXECUTABLE") {
        Some(path) => PathBuf::from(path),
        None => env::current_exe()?
            .parent()
            .context("测试程序缺少父目录")?
            .join(if cfg!(windows) {
                "remoteops-controller-mcp.exe"
            } else {
                "remoteops-controller-mcp"
            }),
    };
    ensure!(
        executable.is_file(),
        "请先构建 remoteops-controller-mcp 或设置 CONTROL_SMOKE_MCP_EXECUTABLE"
    );
    let token = env::var("REMOTEOPS_AI_CONTROLLER_TOKEN").context("缺少测试 AI Token 环境变量")?;
    let owner = env::var("REMOTEOPS_CONTROLLER_OWNER_ID").context("缺少测试 Owner 环境变量")?;
    let config = root.join("mcp.json");
    std::fs::write(&config, "{}")?;
    let mut command = Command::new(executable);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("REMOTEOPS_") {
            command.env_remove(key);
        }
    }
    command
        .arg("--config")
        .arg(config)
        .arg("--relay")
        .arg(address)
        .arg("--server-name")
        .arg("localhost")
        .arg("--ca-cert")
        .arg(cert)
        .arg("--command-mode")
        .arg("agent-controlled")
        .arg("--transfer-root")
        .arg(root.join("mcp-transfers"))
        .arg("--audit-log")
        .arg(root.join("mcp-audit.jsonl"))
        .env("REMOTEOPS_CONTROLLER_TOKEN", token)
        .env("REMOTEOPS_CONTROLLER_OWNER_ID", owner)
        .stderr(Stdio::null())
        .kill_on_drop(true);
    timeout(
        Duration::from_secs(15),
        ().serve(TokioChildProcess::new(command)?),
    )
    .await
    .context("启动真实 MCP 超时")?
    .context("启动真实 MCP 失败")
}

async fn raw_call(client: &Mcp, name: &str, args: Value) -> Result<CallToolResult> {
    let arguments = args.as_object().cloned().context("测试参数必须为对象")?;
    timeout(
        Duration::from_secs(15),
        client.call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments)),
    )
    .await
    .context("MCP 工具调用超时")?
    .context("MCP 工具传输失败")
}

async fn call(client: &Mcp, name: &str, args: Value) -> Result<Value> {
    let result = raw_call(client, name, args).await?;
    ensure!(result.is_error != Some(true), "MCP 工具 {name} 返回失败");
    result.structured_content.context("MCP 工具缺少结构化结果")
}

async fn pairing_code(events: &mpsc::Receiver<AgentEvent>) -> Result<String> {
    timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = events.try_recv() {
                if let AgentEvent::Connected { pairing_code, .. } = event {
                    return Ok(pairing_code);
                }
                if matches!(event, AgentEvent::Failed { .. }) {
                    bail!("真实 Agent 启动失败");
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("等待真实 Agent 控制码超时")?
}

async fn agent_state(
    events: &mpsc::Receiver<AgentEvent>,
    mode: ControllerControlMode,
    source: Option<ControlSource>,
) -> Result<SessionControlState> {
    timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = events.try_recv() {
                if let AgentEvent::ControllerBindingsChanged { bindings } = event
                    && let Some(state) = bindings
                        .into_iter()
                        .filter_map(|binding| binding.control_state)
                        .find(|state| state.mode == mode && state.source == source)
                {
                    return Ok(state);
                }
            }
            sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("等待 Agent 权威授权快照超时")?
}

async fn assert_mcp_mode(
    client: &Mcp,
    session: &str,
    mode: &str,
    source: Option<&str>,
) -> Result<()> {
    timeout(Duration::from_secs(10), async {
        loop {
            let state = call(client, "get_control_mode", json!({"session_id":session})).await?;
            if state["mode"].as_str() == Some(mode) && state["source"].as_str() == source {
                ensure!(state["sync_status"] == "synced", "MCP 未声明状态已同步");
                return Ok(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("真实 MCP 模式未按预期变化")?
}

fn mkdir_args(session: &str, target: &Path) -> Result<Value> {
    let path = target.to_str().context("临时测试路径不是 UTF-8")?;
    let path = if cfg!(windows) {
        path.strip_prefix(r"\\?\").unwrap_or(path)
    } else {
        path
    };
    let (shell, command) = if cfg!(windows) {
        (
            "windows_power_shell",
            format!(
                "New-Item -ItemType Directory -Path '{}' -ErrorAction Stop | Out-Null",
                path.replace('\'', "''")
            ),
        )
    } else {
        (
            "system",
            format!("mkdir -- '{}'", path.replace('\'', "'\\''")),
        )
    };
    Ok(json!({"session_id":session,"shell":shell,"command":command}))
}

async fn re_pair(
    client: &Mcp,
    session: &str,
    code: &str,
    events: &mpsc::Receiver<AgentEvent>,
) -> Result<()> {
    let _ = call(client, "close_connection", json!({"session_id":session})).await?;
    timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = events.try_recv() {
                if matches!(
                    event,
                    AgentEvent::ControllerCountChanged {
                        active_connections: 0
                    }
                ) {
                    return;
                }
            }
            sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("真实 Agent 未确认解绑")?;
    let pair = call(client, "pair_connection", json!({"pairing_code":code})).await?;
    ensure!(
        pair["session_id"].as_str() == Some(session),
        "同一 Agent 重新配对应保持会话标识"
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn scenarios(
    client: &Mcp,
    root: &Path,
    events: &mpsc::Receiver<AgentEvent>,
    control: &AgentPermissionControl,
    default_full: bool,
) -> Result<()> {
    let code = pairing_code(events).await?;
    let paired = call(client, "pair_connection", json!({"pairing_code":code})).await?;
    let session = paired["session_id"]
        .as_str()
        .context("配对结果缺少会话标识")?;
    let mut initial = if default_full {
        let state = agent_state(
            events,
            ControllerControlMode::FullAccess,
            Some(ControlSource::LocalDefault),
        )
        .await?;
        assert_mcp_mode(client, session, "full_access", Some("local_default")).await?;
        state
    } else {
        let _ = agent_state(events, ControllerControlMode::StepByStep, None).await?;
        assert_mcp_mode(client, session, "step_by_step", None).await?;
        let denied = root.join("default-step-must-not-exist");
        let result = raw_call(client, "run_command", mkdir_args(session, &denied)?).await?;
        ensure!(
            result.is_error == Some(true) && !denied.exists(),
            "默认逐项确认必须拒绝无确认写入"
        );
        let result = call(
            client,
            "set_control_mode",
            json!({"session_id":session,"mode":"full_access"}),
        )
        .await?;
        ensure!(
            result["source"] == "mcp" && result["expires_at"].is_string(),
            "MCP 完全控制必须带来源和到期时间"
        );
        assert_mcp_mode(client, session, "full_access", Some("mcp")).await?;
        println!("通过：默认逐项确认拒写，显式 MCP 授权后同步至真实 Agent");
        agent_state(
            events,
            ControllerControlMode::FullAccess,
            Some(ControlSource::Mcp),
        )
        .await?
    };
    let first = root.join("allowed-default");
    let result = call(client, "run_command", mkdir_args(session, &first)?).await?;
    ensure!(
        result["status"] == "completed" && first.is_dir(),
        "默认现场完全控制命令验证失败（status={}，exit_code={}，目录存在={}）",
        result["status"],
        result["exit_code"],
        first.is_dir()
    );
    println!("通过：两端确认完全控制后，真实 Agent 成功执行受控临时命令");

    if default_full {
        re_pair(client, session, &code, events).await?;
        initial = agent_state(
            events,
            ControllerControlMode::FullAccess,
            Some(ControlSource::LocalDefault),
        )
        .await?;
        assert_mcp_mode(client, session, "full_access", Some("local_default")).await?;
        println!("通过：真实解绑重配后，新绑定按保存的默认偏好自动完全控制");
    }
    control.request_control(initial, ControllerControlMode::StepByStep)?;
    let _ = agent_state(events, ControllerControlMode::StepByStep, None).await?;
    assert_mcp_mode(client, session, "step_by_step", None).await?;
    let denied = root.join("must-not-exist");
    let result = raw_call(client, "run_command", mkdir_args(session, &denied)?).await?;
    ensure!(
        result.is_error == Some(true) && !denied.exists(),
        "缺少逐项确认的写命令未被拒绝"
    );
    assert_mcp_mode(client, session, "step_by_step", None).await?;
    println!("通过：现场撤销传播至 MCP；无确认能力时拒绝写操作，默认偏好未恢复授权");

    re_pair(client, session, &code, events).await?;
    let step = agent_state(events, ControllerControlMode::StepByStep, None).await?;
    assert_mcp_mode(client, session, "step_by_step", None).await?;
    let suppressed = root.join("repaired-must-not-exist");
    let result = raw_call(client, "run_command", mkdir_args(session, &suppressed)?).await?;
    ensure!(
        result.is_error == Some(true) && !suppressed.exists(),
        "现场撤销后的新绑定不应恢复自动授权"
    );
    println!("通过：现场撤销后真实解绑重配仍为逐项确认，并继续拒绝无确认写入");
    control.request_control(step, ControllerControlMode::FullAccess)?;
    let _ = agent_state(
        events,
        ControllerControlMode::FullAccess,
        Some(ControlSource::Local),
    )
    .await?;
    assert_mcp_mode(client, session, "full_access", Some("local")).await?;
    let second = root.join("allowed-local");
    let result = call(client, "run_command", mkdir_args(session, &second)?).await?;
    ensure!(
        result["status"] == "completed" && second.is_dir(),
        "现场临时授权后未执行命令"
    );
    ensure!(!denied.exists(), "先前拒绝的命令不得延迟执行");
    println!("通过：现场显式重新授权后允许写操作，先前被拒命令仍未执行");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let address = env::var("CONTROL_SMOKE_ADDRESS").context("缺少本地测试 Relay 地址")?;
    let socket: std::net::SocketAddr = address.parse().context("测试地址必须为本地 IP:端口")?;
    ensure!(
        socket.ip().is_loopback(),
        "真实命令验收只允许连接本机 Relay"
    );
    ensure!(
        !env::var("REMOTEOPS_VISUAL_PROVIDER_ENABLED")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true")),
        "测试需禁用视觉 Provider"
    );
    let cert = PathBuf::from(env::var("CONTROL_SMOKE_CERT").context("缺少测试证书")?);
    let workspace = Workspace::create()?;
    for default_full in [true, false] {
        let root = workspace.0.join(if default_full {
            "default-full"
        } else {
            "default-step"
        });
        std::fs::create_dir(&root)?;
        run_case(&root, &address, &cert, default_full).await?;
    }
    println!("真实 MCP + Agent 控制授权端到端验收通过");
    Ok(())
}

async fn run_case(root: &Path, address: &str, cert: &Path, default_full: bool) -> Result<()> {
    let config = AgentConfig {
        default_full_control: default_full,
        relay: address.to_owned(),
        server_name: "localhost".into(),
        ca_cert: Some(cert.to_owned()),
        transfer_root: root.join("agent-transfers"),
        state_file: root.join("agent-state.json"),
        ..AgentConfig::default()
    };
    let control = AgentPermissionControl::from_config(&config);
    let (events_sender, events) = mpsc::channel();
    let (stop, stopped) = watch::channel(false);
    let agent = tokio::spawn(run_agent_with_permission_control(
        config,
        Some(events_sender),
        stopped,
        control.clone(),
    ));
    let result = async {
        let client = start_mcp(root, address, cert).await?;
        let result = scenarios(&client, root, &events, &control, default_full).await;
        let _ = client.cancel().await;
        result
    }
    .await;
    let _ = stop.send(true);
    let mut agent = agent;
    let shutdown_result = if let Ok(joined) = timeout(Duration::from_secs(10), &mut agent).await {
        joined
            .context("真实 Agent 任务异常终止")
            .and_then(|result| result)
    } else {
        agent.abort();
        let _ = agent.await;
        Err(anyhow::anyhow!("真实 Agent 未在期限内停止"))
    };
    result?;
    shutdown_result?;
    Ok(())
}
