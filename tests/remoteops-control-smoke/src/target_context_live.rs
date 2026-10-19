//! 同一通用 MCP 并连交互与服务目标，确保上下文隔离且信息查询不探测桌面。
use super::{Mcp, call, start_mcp};
use anyhow::{Context, Result, bail, ensure};
use remoteops_domain::{
    AgentBootId, AgentInstanceId, AgentRunMode, Capability, CapabilityAvailability, CapabilitySet,
    EnvironmentProfile, RemoteOperation, TargetCapabilityStatus, TargetContext, TargetGuidance,
    TargetRuntimeFacts,
};
use remoteops_protocol::{
    AgentHello, AgentResumeCommitAck, AgentWelcomeAck, ClientHello, PROTOCOL_VERSION, PairingMode,
    RemoteResponse, WireMessage, connect_tls, load_client_config, read_frame, write_frame,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{task::JoinHandle, time::timeout};

struct SimulatedTarget {
    code: String,
    visual_requests: Arc<AtomicUsize>,
    context_requests: Arc<AtomicUsize>,
    task: JoinHandle<Result<()>>,
}
impl Drop for SimulatedTarget {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn target_context(service: bool) -> TargetContext {
    let product_id = if service {
        "opscenter-service-simulated"
    } else {
        "customer-gui-simulated"
    };
    TargetContext {
        product_id: product_id.to_owned(),
        name: product_id.to_owned(),
        version: "test-1".to_owned(),
        run_mode: if service {
            AgentRunMode::WindowsService
        } else {
            AgentRunMode::Interactive
        },
        facts: TargetRuntimeFacts {
            process_session_id: Some(u32::from(!service)),
            interactive_desktop: if service {
                CapabilityAvailability::Unavailable
            } else {
                CapabilityAvailability::Available
            },
            observed_at: EnvironmentProfile::empty().collected_at,
            source: "host_runtime_simulated".to_owned(),
        },
        capabilities: vec![TargetCapabilityStatus {
            capability: "visual".to_owned(),
            status: if service {
                CapabilityAvailability::Unavailable
            } else {
                CapabilityAvailability::Available
            },
            reasons: if service {
                vec!["product_policy_disabled".to_owned()]
            } else {
                vec![]
            },
        }],
        guidance: vec![TargetGuidance {
            id: "operating-guide".to_owned(),
            version: "1".to_owned(),
            source: "built_in_product_simulated".to_owned(),
            text: if service {
                "SERVICE_ONLY_GUIDANCE"
            } else {
                "GUI_ONLY_GUIDANCE"
            }
            .to_owned(),
            references: vec![],
        }],
        ..TargetContext::default()
    }
}

async fn start_target(address: &str, cert: &Path, service: bool) -> Result<SimulatedTarget> {
    start_target_variant(address, cert, service, PROTOCOL_VERSION, true).await
}

#[allow(clippy::too_many_lines)]
async fn start_target_variant(
    address: &str,
    cert: &Path,
    service: bool,
    version: u16,
    report_context: bool,
) -> Result<SimulatedTarget> {
    let context = target_context(service);
    context.validate().map_err(anyhow::Error::msg)?;
    let mut stream = connect_tls(address, "localhost", load_client_config(cert)?).await?;
    let mut environment = EnvironmentProfile::empty();
    "windows".clone_into(&mut environment.os_family);
    environment.protocol_version = PROTOCOL_VERSION;
    let capabilities = if service {
        CapabilitySet::new([Capability::Cmd])
    } else {
        CapabilitySet::new([Capability::Cmd, Capability::Visual])
    };
    write_frame(
        &mut stream,
        &WireMessage::Hello(ClientHello::Agent(AgentHello {
            protocol_version: version,
            agent_instance_id: AgentInstanceId::new(),
            boot_id: AgentBootId::new(),
            target_context: report_context.then_some(context.clone()),
            pairing_mode: PairingMode::AutoPerBoot,
            managed_enrollment: None,
            resume_token: None,
            hostname: context.product_id.clone(),
            operating_system: "Windows simulated".to_owned(),
            capabilities,
            environment,
            credential_encryption_public_key: String::new(),
            credential_encryption_key_id: String::new(),
            mac_address: None,
            supports_agent_shutdown: false,
        })),
    )
    .await?;
    let message: WireMessage = timeout(Duration::from_secs(10), read_frame(&mut stream)).await??;
    let WireMessage::AgentWelcome(welcome) = message else {
        bail!("模拟目标未收到Welcome")
    };
    let code = welcome
        .pairing_code
        .as_ref()
        .context("本地交互模式模拟目标必须返回控制码")?
        .to_string();
    write_frame(
        &mut stream,
        &WireMessage::AgentWelcomeAck(AgentWelcomeAck {
            connection_generation: welcome.connection_generation,
        }),
    )
    .await?;
    let message: WireMessage = timeout(Duration::from_secs(10), read_frame(&mut stream)).await??;
    ensure!(
        matches!(message, WireMessage::AgentResumeCommitted(_)),
        "模拟目标握手未提交"
    );
    write_frame(
        &mut stream,
        &WireMessage::AgentResumeCommitAck(AgentResumeCommitAck {
            connection_generation: welcome.connection_generation,
        }),
    )
    .await?;
    let visual_requests = Arc::new(AtomicUsize::new(0));
    let context_requests = Arc::new(AtomicUsize::new(0));
    let visual_count = visual_requests.clone();
    let context_count = context_requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let message: WireMessage = read_frame(&mut stream).await?;
            if let WireMessage::AuthorizedRemoteRequest(authorized) = message {
                let request = authorized.request;
                let mut error_code = None;
                let details = match request.operation {
                    RemoteOperation::GetTargetContext => {
                        context_count.fetch_add(1, Ordering::SeqCst);
                        Some(serde_json::to_value(&context)?)
                    }
                    operation if operation.is_visual() => {
                        visual_count.fetch_add(1, Ordering::SeqCst);
                        if service {
                            error_code = Some("product_policy_disabled".to_owned());
                        }
                        Some(
                            json!({"state":"ready","displays":[{"id":"simulated"}],"windows":[{"id":"simulated"}],
                            "active_window_fingerprint":"simulated-window", "ui_tree":{"available":true},"screenshot_base64":"c2ltdWxhdGVk"}),
                        )
                    }
                    _ => {
                        error_code = Some("unsupported_simulated_operation".to_owned());
                        None
                    }
                };
                write_frame(
                    &mut stream,
                    &WireMessage::RemoteResponse(RemoteResponse {
                        request_id: request.request_id,
                        session_id: request.session_id,
                        exit_code: Some(0),
                        summary: "本地模拟目标响应；未操作实际桌面".to_owned(),
                        error_code,
                        payload_base64: None,
                        sha256: None,
                        details,
                    }),
                )
                .await?;
            }
        }
    });
    Ok(SimulatedTarget {
        code,
        visual_requests,
        context_requests,
        task,
    })
}

fn assert_context(value: &Value, service: bool) -> Result<()> {
    let expected = if service {
        "opscenter-service-simulated"
    } else {
        "customer-gui-simulated"
    };
    ensure!(
        value["target_context"]["product_id"] == expected,
        "目标产品上下文发生串用"
    );
    ensure!(
        value["target_context"]["guidance"][0]["text"]
            == if service {
                "SERVICE_ONLY_GUIDANCE"
            } else {
                "GUI_ONLY_GUIDANCE"
            },
        "目标专用指引发生串用"
    );
    ensure!(
        value["guidance_is_advisory"] == true,
        "指引未标记为辅助资料"
    );
    Ok(())
}

async fn scenarios(client: &Mcp, gui: &SimulatedTarget, service: &SimulatedTarget) -> Result<()> {
    let gui_pair = call(
        client,
        "pair_connection",
        json!({"pairing_code":gui.code,"alias":"gui-context"}),
    )
    .await?;
    let service_pair = call(
        client,
        "pair_connection",
        json!({"pairing_code":service.code,"alias":"service-context"}),
    )
    .await?;
    assert_context(&gui_pair, false)?;
    assert_context(&service_pair, true)?;
    let gui_session = gui_pair["session_id"].as_str().context("GUI模拟会话缺失")?;
    let service_session = service_pair["session_id"]
        .as_str()
        .context("Service模拟会话缺失")?;
    ensure!(gui_session != service_session, "目标必须使用不同会话");
    for (session, is_service) in [
        (gui_session, false),
        (service_session, true),
        (gui_session, false),
    ] {
        let info = call(client, "get_target_info", json!({"session_id":session})).await?;
        assert_context(&info, is_service)?;
        ensure!(
            info["target_context_status"] == "reported",
            "安全上下文实时刷新失败"
        );
    }
    ensure!(
        gui.context_requests.load(Ordering::SeqCst) >= 2
            && service.context_requests.load(Ordering::SeqCst) >= 1,
        "目标详情没有使用安全上下文查询路径"
    );
    ensure!(
        gui.visual_requests.load(Ordering::SeqCst) == 0
            && service.visual_requests.load(Ordering::SeqCst) == 0,
        "get_target_info错误地探测了图形Provider"
    );
    let list = call(client, "list_connections", json!({})).await?;
    for value in list["connections"].as_array().context("缺少连接列表")? {
        if value["session_id"] == gui_session {
            assert_context(value, false)?;
        }
        if value["session_id"] == service_session {
            assert_context(value, true)?;
        }
    }
    let unavailable = call(
        client,
        "desktop_capabilities",
        json!({"session_id":service_session}),
    )
    .await?;
    ensure!(
        unavailable["status"] == "unavailable"
            && unavailable["details"]["code"] == "product_policy_disabled",
        "服务目标未返回稳定图形禁用原因"
    );
    ensure!(
        unavailable["request_id"].is_null() && unavailable["details"]["observation"].is_null(),
        "服务能力查询不应发起探测或采集观察"
    );
    ensure!(
        service.visual_requests.load(Ordering::SeqCst) == 0,
        "服务目标接收到图形请求"
    );
    let available = call(
        client,
        "desktop_capabilities",
        json!({"session_id":gui_session}),
    )
    .await?;
    ensure!(
        available["details"]["capabilities"]["visual"] == true,
        "独立GUI目标图形能力被服务策略污染"
    );
    ensure!(
        gui.visual_requests.load(Ordering::SeqCst) == 1,
        "显式GUI能力查询应恰好访问模拟Provider一次"
    );
    ensure!(
        service.visual_requests.load(Ordering::SeqCst) == 0,
        "GUI查询影响了服务目标"
    );
    Ok(())
}

pub(super) async fn run(root: &Path, address: &str, cert: &Path) -> Result<()> {
    let gui = start_target(address, cert, false).await?;
    let service = start_target(address, cert, true).await?;
    let legacy = start_target_variant(address, cert, false, 17, true).await?;
    let undeclared = start_target_variant(address, cert, false, PROTOCOL_VERSION, false).await?;
    let client = start_mcp(root, address, cert).await?;
    let result = async {
        scenarios(&client, &gui, &service).await?;
        for target in [&legacy, &undeclared] {
            let paired = call(
                &client,
                "pair_connection",
                json!({"pairing_code":target.code}),
            )
            .await?;
            let session = paired["session_id"]
                .as_str()
                .context("兼容目标缺少session")?;
            let info = call(&client, "get_target_info", json!({"session_id":session})).await?;
            ensure!(
                info["target_context_status"] == "unknown",
                "旧协议或未声明上下文必须显示未知"
            );
            ensure!(
                target.context_requests.load(Ordering::SeqCst) == 0
                    && target.visual_requests.load(Ordering::SeqCst) == 0,
                "旧协议或缺上下文目标不能收到GetTargetContext或图形探测"
            );
        }
        println!(
            "通过：协议17（即使携带附加元数据）及未声明上下文目标不接收新上下文操作，均显示未知"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = client.cancel().await;
    result?;
    println!(
        "通过：真实通用MCP并连模拟GUI/Service上下文隔离，详情查询不探测桌面，服务硬禁无截图请求"
    );
    Ok(())
}
