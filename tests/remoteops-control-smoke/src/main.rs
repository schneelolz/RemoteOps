//! 通过本地真实 TLS Relay 验证控制授权，不执行现场命令。
use anyhow::{Context, Result, bail, ensure};
use remoteops_application::{RelayClient, RelayClientConfig};
use remoteops_domain::{
    AgentInstanceId, Capability, CapabilitySet, ControllerInstanceId, EnvironmentProfile,
    EventSource, PermissionMode, RemoteOperation, RequestId, SessionId, ShellKind,
};
use remoteops_protocol::{
    AgentHello, AgentPermissionModeChanged, AgentResumeCommitAck, AgentWelcome, AgentWelcomeAck,
    ClientHello, ControlBasis, ControlProof, ControlSource, ControlStateRequest,
    ControllerControlMode, ControllerKind, PROTOCOL_VERSION, RemoteResponse, SessionControlState,
    WireMessage, connect_tls, load_client_config, read_frame, write_frame,
};
use std::{env, path::PathBuf, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_rustls::client::TlsStream;

type Stream = TlsStream<TcpStream>;

async fn receive(stream: &mut Stream) -> Result<WireMessage> {
    Ok(timeout(Duration::from_secs(8), read_frame(stream))
        .await
        .context("Agent 等待消息超时")??)
}

async fn agent_connect(
    address: &str,
    cert: &str,
    id: AgentInstanceId,
    token: Option<String>,
) -> Result<(Stream, AgentWelcome)> {
    let initial = token.is_none();
    let mut stream = connect_tls(address, "localhost", load_client_config(cert)?).await?;
    write_frame(
        &mut stream,
        &WireMessage::Hello(ClientHello::Agent(AgentHello {
            protocol_version: PROTOCOL_VERSION,
            agent_instance_id: id,
            resume_token: token,
            hostname: "control-smoke-simulated-agent".into(),
            operating_system: "simulated".into(),
            capabilities: CapabilitySet::new([Capability::Cmd]),
            environment: EnvironmentProfile::default(),
            credential_encryption_public_key: String::new(),
            credential_encryption_key_id: String::new(),
            mac_address: None,
            supports_agent_shutdown: false,
        })),
    )
    .await?;
    let WireMessage::AgentWelcome(welcome) = receive(&mut stream).await? else {
        bail!("未收到 AgentWelcome")
    };
    write_frame(
        &mut stream,
        &WireMessage::AgentWelcomeAck(AgentWelcomeAck {
            connection_generation: welcome.connection_generation,
        }),
    )
    .await?;
    let WireMessage::AgentResumeCommitted(_) = receive(&mut stream).await? else {
        bail!("未确认恢复令牌")
    };
    write_frame(
        &mut stream,
        &WireMessage::AgentResumeCommitAck(AgentResumeCommitAck {
            connection_generation: welcome.connection_generation,
        }),
    )
    .await?;
    // 仅模拟 Agent 声明允许控制端逐项确认，不执行系统操作。
    if initial {
        write_frame(
            &mut stream,
            &WireMessage::AgentPermissionModeChanged(AgentPermissionModeChanged {
                permission_mode: PermissionMode::ControllerApproved,
            }),
        )
        .await?;
    }
    Ok((stream, welcome))
}

async fn agent_state(stream: &mut Stream, expected: &SessionControlState) -> Result<()> {
    loop {
        if let WireMessage::ControlStateUpdated(state) = receive(stream).await?
            && state.revision == expected.revision
        {
            ensure!(state == *expected, "Agent 与 MCP 快照不一致");
            return Ok(());
        }
    }
}

async fn local_mode(
    stream: &mut Stream,
    old: &SessionControlState,
    mode: ControllerControlMode,
) -> Result<SessionControlState> {
    let request_id = RequestId::new();
    write_frame(
        stream,
        &WireMessage::ControlStateRequest(ControlStateRequest {
            request_id,
            session_id: old.session_id,
            expected: Some(old.clone()),
            mode: Some(mode),
            source: Some(ControlSource::Local),
        }),
    )
    .await?;
    loop {
        if let WireMessage::ControlStateResult(result) = receive(stream).await?
            && result.request_id == request_id
        {
            ensure!(result.error.is_none(), "现场变更被拒绝：{:?}", result.error);
            return result.state.context("现场变更缺少状态");
        }
    }
}

fn operation() -> RemoteOperation {
    RemoteOperation::RunCommand {
        shell: ShellKind::Cmd,
        command: "mkdir control-smoke-simulated-only".into(),
        readonly: false,
    }
}

async fn fake_success(stream: &mut Stream) -> Result<()> {
    loop {
        if let WireMessage::AuthorizedRemoteRequest(authorized) = receive(stream).await? {
            let request = authorized.request;
            write_frame(
                stream,
                &WireMessage::RemoteResponse(RemoteResponse {
                    request_id: request.request_id,
                    session_id: request.session_id,
                    exit_code: Some(0),
                    summary: "模拟成功；未执行命令".into(),
                    error_code: None,
                    payload_base64: None,
                    sha256: None,
                    details: None,
                }),
            )
            .await?;
            return Ok(());
        }
    }
}

// 单独验证现场授权与请求转发，便于定位协议模拟失败阶段。
async fn verify_local_execution(
    client: &RelayClient,
    agent: &mut Stream,
    session: SessionId,
    revoked: &SessionControlState,
) -> Result<SessionControlState> {
    let local = local_mode(agent, revoked, ControllerControlMode::FullAccess).await?;
    println!("检查：查询 Relay 当前快照");
    let queried = client.query_control_state(session).await?;
    ensure!(
        queried == local
            && local.source == Some(ControlSource::Local)
            && local.expires_at.is_none(),
        "现场授权状态错误"
    );
    let target = session.to_string();
    let execute = client.execute_with_control(
        &target,
        EventSource::Ai,
        operation(),
        None,
        None,
        None,
        Some(ControlProof {
            state: local.clone(),
            basis: ControlBasis::FullAccess,
        }),
    );
    let (executed, response) = tokio::join!(execute, fake_success(agent));
    response?;
    executed?;
    println!("通过：现场完全控制允许修改请求转发（模拟响应，无系统写入）");
    Ok(local)
}

// 使用独立 AI 身份连接本地测试中继。
async fn controller_connect(address: &str, cert: &str) -> Result<RelayClient> {
    RelayClient::connect(
        RelayClientConfig {
            relay_address: address.to_owned(),
            server_name: "localhost".into(),
            ca_certificate: Some(PathBuf::from(cert)),
            tls_fingerprint: None,
            audit_log: None,
            controller_kind: ControllerKind::Ai,
            owner_id: env::var("REMOTEOPS_CONTROLLER_OWNER_ID")?.parse()?,
            permission_mode: PermissionMode::ControllerApproved,
            authentication_token: env::var("REMOTEOPS_AI_CONTROLLER_TOKEN")?,
            reconnect_delay: Duration::from_millis(100),
        },
        ControllerInstanceId::new(),
    )
    .await
    .map_err(Into::into)
}

async fn run() -> Result<()> {
    let address = env::var("CONTROL_SMOKE_ADDRESS")?;
    ensure!(address.starts_with("127.0.0.1:"), "只允许本地回环 Relay");
    let cert = env::var("CONTROL_SMOKE_CERT")?;
    let agent_id = AgentInstanceId::new();
    let (mut agent, welcome) = agent_connect(&address, &cert, agent_id, None).await?;
    let client = controller_connect(&address, &cert).await?;
    let connection = client.pair(welcome.pairing_code.clone()).await?;
    let session = connection.session_id;
    let step = client.query_control_state(session).await?;
    ensure!(
        step.mode == ControllerControlMode::StepByStep,
        "配对默认不是逐项确认"
    );
    agent_state(&mut agent, &step).await?;
    println!("通过：TLS 配对默认逐项确认，双端快照一致");
    let result = client
        .request_control_state(ControlStateRequest {
            request_id: RequestId::new(),
            session_id: session,
            expected: Some(step),
            mode: Some(ControllerControlMode::FullAccess),
            source: Some(ControlSource::Mcp),
        })
        .await?;
    let full = result;
    ensure!(
        full.mode == ControllerControlMode::FullAccess && full.source == Some(ControlSource::Mcp),
        "MCP 授权来源错误"
    );
    agent_state(&mut agent, &full).await?;
    println!("通过：MCP 完全控制同步到 Agent");
    let revoked = local_mode(&mut agent, &full, ControllerControlMode::StepByStep).await?;
    println!("检查：查询 Relay 当前快照");
    let queried = client.query_control_state(session).await?;
    ensure!(
        queried == revoked && revoked.mode == ControllerControlMode::StepByStep,
        "撤销未同步至 MCP"
    );
    println!("检查：发送旧授权请求，预期立即拒绝");
    let rejected_request = client
        .execute_with_control(
            &session.to_string(),
            EventSource::Ai,
            operation(),
            None,
            None,
            None,
            Some(ControlProof {
                state: full,
                basis: ControlBasis::FullAccess,
            }),
        )
        .await;
    ensure!(
        matches!(rejected_request, Err(remoteops_application::ApplicationError::Remote { ref code, .. }) if code == "control_authorization_required"),
        "旧完全控制凭据未被授权校验明确拒绝"
    );
    println!("通过：现场撤销同步至 MCP，旧请求被拒绝");
    let local = verify_local_execution(&client, &mut agent, session, &revoked).await?;
    drop(agent);
    let (mut resumed, _) =
        agent_connect(&address, &cert, agent_id, Some(welcome.resume_token)).await?;
    let reset = loop {
        match receive(&mut resumed).await? {
            WireMessage::ControlStateUpdated(state) => break state,
            WireMessage::ControllerBinding(binding) if binding.control_state.is_some() => {
                break binding.control_state.context("恢复绑定缺少状态")?;
            }
            _ => {}
        }
    };
    ensure!(
        reset.agent_generation != local.agent_generation
            && reset.mode == ControllerControlMode::StepByStep
            && reset.source.is_none(),
        "重连恢复了旧授权"
    );
    println!("检查：查询 Relay 当前快照");
    let queried = client.query_control_state(session).await?;
    ensure!(
        queried == reset,
        "重连后双端状态不一致：Agent rev={} mode={:?} gen={}；MCP rev={} mode={:?} gen={}",
        reset.revision,
        reset.mode,
        reset.agent_generation,
        queried.revision,
        queried.mode,
        queried.agent_generation
    );
    println!("通过：真实 TLS 断线重连清除现场临时授权");
    println!("协议模拟子测试结束；默认自动授权与真实写操作由后续 live 子测试验证");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    timeout(Duration::from_secs(45), run())
        .await
        .context("控制权限专项测试总超时")?
}
