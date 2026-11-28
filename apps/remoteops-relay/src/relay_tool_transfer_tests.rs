// 通过已认证帧流维护真实 ZIP，逐块确认并验证运行开关与权限。
fn tool_transfer_zip() -> Vec<u8> {
    use rand::{RngCore as _, SeedableRng as _};

    let mut payload = vec![0_u8; 1024 * 1024 + 137];
    rand::rngs::StdRng::seed_from_u64(0x524f_5443).fill_bytes(&mut payload);
    let mut crc = u32::MAX;
    for byte in &payload {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    let crc = !crc;
    let size = u32::try_from(payload.len()).unwrap();
    let name = b"tool.exe";
    let mut zip = Vec::new();
    let short = |bytes: &mut Vec<u8>, value: u16| bytes.extend_from_slice(&value.to_le_bytes());
    let long = |bytes: &mut Vec<u8>, value: u32| bytes.extend_from_slice(&value.to_le_bytes());
    long(&mut zip, 0x0403_4b50);
    for field in [20, 0, 0, 0, 0] {
        short(&mut zip, field);
    }
    for field in [crc, size, size] {
        long(&mut zip, field);
    }
    short(&mut zip, u16::try_from(name.len()).unwrap());
    short(&mut zip, 0);
    zip.extend_from_slice(name);
    zip.extend_from_slice(&payload);
    let directory_offset = u32::try_from(zip.len()).unwrap();
    long(&mut zip, 0x0201_4b50);
    for field in [20, 20, 0, 0, 0, 0] {
        short(&mut zip, field);
    }
    for field in [crc, size, size] {
        long(&mut zip, field);
    }
    short(&mut zip, u16::try_from(name.len()).unwrap());
    for _ in 0..4 {
        short(&mut zip, 0);
    }
    long(&mut zip, 0);
    long(&mut zip, 0);
    zip.extend_from_slice(name);
    let directory_size = u32::try_from(zip.len()).unwrap() - directory_offset;
    long(&mut zip, 0x0605_4b50);
    for field in [0, 0, 1, 1] {
        short(&mut zip, field);
    }
    long(&mut zip, directory_size);
    long(&mut zip, directory_offset);
    short(&mut zip, 0);
    zip
}

async fn tool_transfer_command(
    client: &mut tokio::io::DuplexStream,
    revision: u64,
    command: remoteops_tool_center::ToolCenterCommand,
) -> remoteops_tool_center::ToolCenterResponse {
    let request = remoteops_tool_center::ToolCenterRequest {
        request_id: Uuid::new_v4().to_string(),
        expected_revision: revision,
        command,
    };
    let id = request.request_id.clone();
    write_frame(
        client,
        &WireMessage::ToolCenterRequest {
            request_id: id.clone(),
            request: serde_json::to_value(request).unwrap(),
        },
    )
    .await
    .unwrap();
    write_frame(
        client,
        &WireMessage::Heartbeat {
            sent_at: Utc::now(),
            controller_modes: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut heartbeat = false;
    let mut response = None;
    while !heartbeat || response.is_none() {
        match time::timeout(
            time::Duration::from_secs(5),
            read_frame::<WireMessage, _>(client),
        )
        .await
        .expect("大包管理期间必须继续处理心跳")
        .unwrap()
        {
            WireMessage::HeartbeatAck { .. } => heartbeat = true,
            WireMessage::ToolCenterResponse {
                request_id,
                response: payload,
            } => {
                assert_eq!(request_id, id);
                response = Some(serde_json::from_value(payload).unwrap());
            }
            message => panic!("意外的工具传输消息: {message:?}"),
        }
    }
    response.unwrap()
}

fn tool_transfer_definition(size: u64, sha256: String) -> remoteops_tool_center::ManagedTool {
    use remoteops_tool_center::{ManagedTool, PackageSource, ToolPackage, ToolVersion};

    let index: remoteops_resources::ResourceIndex = serde_json::from_slice(include_bytes!(
        "../../../examples/resource-site/remoteops/v1/index.json"
    ))
    .unwrap();
    let mut descriptor = index.tools.into_iter().next().unwrap();
    descriptor.id = "transfer-fixture".into();
    descriptor.version = "1.0.0".into();
    descriptor.package.size = size;
    descriptor.package.sha256 = sha256;
    descriptor.startup.executable = "tool.exe".into();
    descriptor.startup.browser_executable = "tool.exe".into();
    descriptor.requires_desktop = false;
    ManagedTool {
        id: descriptor.id.clone(),
        name: "跨端包传输测试".into(),
        purpose: "验证中继托管传输".into(),
        instructions: "该随机包仅用于传输验证，不执行".into(),
        examples: vec!["256 KiB 分块".into()],
        dependencies: Vec::new(),
        enabled: true,
        versions: vec![ToolVersion {
            version: descriptor.version.clone(),
            packages: vec![ToolPackage {
                tool: descriptor,
                source: PackageSource::Hosted,
                file_verified: false,
                tests: Vec::new(),
            }],
        }],
    }
}

async fn tool_transfer_download(
    relay: &Relay,
    agent: AgentInstanceId,
    generation: u64,
    sha256: &str,
    offset: u64,
) -> WireMessage {
    let (sender, mut receiver) = test_channel();
    relay.send_tool_package(
        agent,
        generation,
        Uuid::new_v4().to_string(),
        sha256.into(),
        offset,
        sender,
        Arc::new(tokio::sync::Semaphore::new(1)),
    );
    time::timeout(time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn tool_transfer_authenticated_megabyte_upload_download_heartbeat_disable_and_revoke() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use remoteops_tool_center::{PACKAGE_CHUNK_SIZE, ToolCenter, ToolCenterCommand as Command};

    let (mut relay, installation, token) = enrolled_relay();
    let directory = TestStateFile::new("tool-transfer-e2e");
    relay.tool_center = Some(Arc::new(ToolCenter::open(&directory.root).unwrap()));
    relay
        .set_mcp_tool_management(installation, true)
        .await
        .unwrap();
    let relay = Arc::new(relay);
    let (server, mut client) = tokio::io::duplex(1024 * 1024);
    let server_task = tokio::spawn({
        let relay = Arc::clone(&relay);
        async move { relay.handle_client(server).await }
    });
    let hello = installation_hello(token);
    let controller = hello.controller_instance_id;
    write_frame(
        &mut client,
        &WireMessage::Hello(ClientHello::Controller(hello)),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<WireMessage, _>(&mut client).await.unwrap(),
        WireMessage::ControllerWelcome { .. }
    ));
    assert!(
        relay.state.lock().await.controllers[&controller]
            .sessions
            .is_empty()
    );

    let archive = tool_transfer_zip();
    assert!(archive.len() > 1024 * 1024);
    let sha256 = format!("{:x}", Sha256::digest(&archive));
    tool_transfer_upload(&mut client, &archive, &sha256).await;
    assert!(!relay.tool_center().unwrap().cached_catalog().enabled);
    let revision = tool_transfer_register(&relay, &mut client, archive.len() as u64, &sha256).await;
    let agent = AgentInstanceId::new();
    let (registration, _agent_messages, _session) = ready_agent(&relay, agent, None).await;
    let generation = registration.connection_generation;
    // 模拟旧 Agent 与新版 Agent 的能力声明；显式适配不能被旧端静默忽略。
    let center = relay.tool_center().unwrap();
    let mut definition = center.snapshot().unwrap().tools.into_iter().next().unwrap();
    definition.versions[0].packages[0]
        .tool
        .startup
        .browser_adapter = Some(remoteops_resources::BrowserAdapter::Playwright);
    let updated = relay
        .admin_tools_request(
            "admin:test",
            remoteops_tool_center::ToolCenterRequest {
                request_id: Uuid::new_v4().to_string(),
                expected_revision: revision,
                command: Command::PutTool { tool: definition },
            },
        )
        .await;
    assert!(updated.error.is_none());
    let revision = updated.revision;
    {
        let mut state = relay.state.lock().await;
        let request = RemoteRequest {
            request_id: RequestId::new(),
            session_id: state.agents[&agent].session_id,
            operation: RemoteOperation::StartResourceTool {
                tool_id: "transfer-fixture".into(),
                version: "1.0.0".into(),
                sha256: sha256.clone(),
            },
            source: remoteops_domain::EventSource::Ai,
            approval_id: None,
            payload_base64: None,
            control_proof: None,
        };
        assert_eq!(
            relay.check_tool_request(&state, &request).unwrap_err().0,
            "browser_adapter_unsupported"
        );
        let context = state
            .agents
            .get_mut(&agent)
            .unwrap()
            .hello
            .target_context
            .get_or_insert_with(remoteops_domain::TargetContext::default);
        context
            .capabilities
            .push(remoteops_domain::TargetCapabilityStatus {
                capability: "resource_tools_browser_adapters_v1".into(),
                status: remoteops_domain::CapabilityAvailability::Available,
                reasons: vec![],
            });
        assert!(relay.check_tool_request(&state, &request).is_ok());
    }
    let revision = tool_transfer_verify_download(
        &relay,
        &mut client,
        agent,
        generation,
        &sha256,
        &archive,
        revision,
    )
    .await;
    let second = tool_transfer_command(
        &mut client,
        revision,
        Command::BeginUpload {
            size: archive.len() as u64,
            sha256,
        },
    )
    .await;
    assert!(second.error.is_none(), "全局关闭不影响工具库维护");
    relay
        .set_mcp_tool_management(installation, false)
        .await
        .unwrap();
    let denied = tool_transfer_command(
        &mut client,
        revision,
        Command::UploadChunk {
            upload_id: second.data["upload_id"].as_str().unwrap().into(),
            offset: 0,
            data_base64: STANDARD.encode(&archive[..PACKAGE_CHUNK_SIZE]),
        },
    )
    .await;
    assert_eq!(denied.error.unwrap().code, "tool_management_denied");
    server_task.abort();
    let _ = server_task.await;
}

async fn tool_transfer_upload(client: &mut tokio::io::DuplexStream, archive: &[u8], sha256: &str) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use remoteops_tool_center::{PACKAGE_CHUNK_SIZE, ToolCenterCommand as Command};
    let begun = tool_transfer_command(
        client,
        0,
        Command::BeginUpload {
            size: archive.len() as u64,
            sha256: sha256.into(),
        },
    )
    .await;
    assert!(begun.error.is_none());
    let upload = begun.data["upload_id"].as_str().unwrap().to_string();
    let mut offset = 0_u64;
    for bytes in archive.chunks(PACKAGE_CHUNK_SIZE) {
        let response = tool_transfer_command(
            client,
            0,
            Command::UploadChunk {
                upload_id: upload.clone(),
                offset,
                data_base64: STANDARD.encode(bytes),
            },
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        offset += bytes.len() as u64;
        assert_eq!(response.data["offset"], offset);
    }
    let uploaded =
        tool_transfer_command(client, 0, Command::FinishUpload { upload_id: upload }).await;
    assert!(uploaded.error.is_none(), "{:?}", uploaded.error);
    assert_eq!(uploaded.data["sha256"], sha256);
    assert_eq!(uploaded.data["size"], archive.len());
    assert_eq!(uploaded.data["file_verified"], true);
}

async fn tool_transfer_register(
    relay: &Relay,
    client: &mut tokio::io::DuplexStream,
    size: u64,
    sha256: &str,
) -> u64 {
    use remoteops_tool_center::{PlatformTest, ToolCenterCommand as Command};
    let descriptor = tool_transfer_definition(size, sha256.into());
    let saved = tool_transfer_command(client, 0, Command::PutTool { tool: descriptor }).await;
    assert!(saved.error.is_none(), "{:?}", saved.error);
    let snapshot = relay.tool_center().unwrap().snapshot().unwrap();
    assert!(!snapshot.tools[0].enabled);
    assert_eq!(
        snapshot.tools[0].versions[0].packages[0].tool.platforms[0].status,
        "pending"
    );
    let tested = tool_transfer_command(
        client,
        saved.revision,
        Command::RecordTest {
            tool_id: "transfer-fixture".into(),
            version: "1.0.0".into(),
            sha256: sha256.into(),
            record: PlatformTest {
                os: "windows".into(),
                arch: "x86_64".into(),
                environment: "合成传输测试平台；未执行随机包".into(),
                desktop: false,
                windows_build: Some(26100),
                status: "pending".into(),
                result: "ZIP完整性和分块摘要已核对，不声明平台运行通过".into(),
                tested_at: Utc::now().to_rfc3339(),
                recorded_by: String::new(),
            },
        },
    )
    .await;
    assert!(tested.error.is_none());
    let enabled = tool_transfer_command(
        client,
        tested.revision,
        Command::SetToolEnabled {
            tool_id: "transfer-fixture".into(),
            enabled: true,
        },
    )
    .await;
    assert!(enabled.error.is_none());
    let policy = tool_transfer_command(
        client,
        enabled.revision,
        Command::SetPolicy { enabled: true },
    )
    .await;
    assert!(policy.error.is_none());
    policy.revision
}

// 每个下载请求确认上一块，期间通过同一已认证控制器读取策略并处理心跳。
async fn tool_transfer_verify_download(
    relay: &Relay,
    client: &mut tokio::io::DuplexStream,
    agent: AgentInstanceId,
    generation: u64,
    sha256: &str,
    archive: &[u8],
    revision: u64,
) -> u64 {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use remoteops_tool_center::{PACKAGE_CHUNK_SIZE, ToolCenterCommand as Command};
    assert!(matches!(
        tool_transfer_download(relay, agent, generation, sha256, 0).await,
        WireMessage::ToolPackageChunk {
            error: None,
            eof: false,
            ..
        }
    ));

    let disabled = tool_transfer_command(
        client,
        revision,
        Command::SetToolEnabled {
            tool_id: "transfer-fixture".into(),
            enabled: false,
        },
    )
    .await;
    assert!(disabled.error.is_none());
    assert!(matches!(
        tool_transfer_download(relay, agent, generation, sha256, PACKAGE_CHUNK_SIZE as u64).await,
        WireMessage::ToolPackageChunk { error: Some(_), .. }
    ));
    let enabled = tool_transfer_command(
        client,
        disabled.revision,
        Command::SetToolEnabled {
            tool_id: "transfer-fixture".into(),
            enabled: true,
        },
    )
    .await;
    assert!(enabled.error.is_none());
    let mut received = Vec::new();
    loop {
        let WireMessage::ToolPackageChunk {
            offset,
            data_base64,
            eof,
            error,
            ..
        } = tool_transfer_download(relay, agent, generation, sha256, received.len() as u64).await
        else {
            panic!("下载必须返回独立分块");
        };
        assert!(error.is_none());
        assert_eq!(offset, received.len() as u64);
        let bytes = STANDARD.decode(data_base64).unwrap();
        assert!(bytes.len() <= PACKAGE_CHUNK_SIZE);
        received.extend_from_slice(&bytes);
        let snapshot = tool_transfer_command(client, enabled.revision, Command::Snapshot).await;
        assert!(
            snapshot.error.is_none(),
            "下载期间心跳和管理查询必须继续处理"
        );
        if eof {
            break;
        }
    }
    assert_eq!(format!("{:x}", Sha256::digest(&received)), sha256);
    assert_eq!(received, archive);
    let global = tool_transfer_command(
        client,
        enabled.revision,
        Command::SetPolicy { enabled: false },
    )
    .await;
    assert!(global.error.is_none());
    assert!(matches!(
        tool_transfer_download(relay, agent, generation, sha256, 0).await,
        WireMessage::ToolPackageChunk { error: Some(_), .. }
    ));

    global.revision
}
