#[tokio::test]
async fn paired_desktop_is_human_only_and_bound_to_the_live_stream() {
    verify_paired_desktop(false).await;
}

#[tokio::test]
#[ignore = "通过 scripts/crossdesk-lab/Invoke-SignalFixture.ps1 启动私有信令和原生对端"]
async fn crossdesk_native_media_and_handoff() {
    verify_paired_desktop(true).await;
}

async fn verify_paired_desktop(native: bool) {
    use remoteops_protocol::desktop::{Action, DesktopEvent, Input, read_video, write_video};
    let turn_config = std::env::temp_dir().join(format!("remoteops-turn-test-{}.json", uuid::Uuid::new_v4()));
    let config = serde_json::json!({"address":"192.0.2.1","port":7443,"secret":"ab".repeat(32)});
    std::fs::write(&turn_config, serde_json::to_vec(&config).unwrap()).unwrap();
    let issuer = crate::turn::Issuer::load(&turn_config).unwrap();
    std::fs::remove_file(turn_config).unwrap();
    let relay = std::sync::Arc::new(relay(Duration::minutes(10)).with_turn(Some(issuer)));
    let aid = AgentInstanceId::new();
    let (agent, mut ae, session) = ready_agent(&relay, aid, None).await;
    let ag = agent.connection_generation;
    relay
        .state
        .lock()
        .await
        .agents
        .get_mut(&aid)
        .unwrap()
        .hello
        .environment
        .tools
        .push(remoteops_domain::ToolProfile {
            name: "remoteops-desktop-v1".into(),
            available: true,
            version: None,
        });
    let (human, hg, mut he) = register_controller(&relay, ControllerKind::Human).await;
    let (ai, ig, mut ie) = register_controller(&relay, ControllerKind::Ai).await;
    for (id, generation) in [(human, hg), (ai, ig)] {
        let response = relay
            .pair_controller(
                id,
                generation,
                PairRequest {
                    expected_agent_id: None,
                    expected_session_id: None,
                    connection_resume_token: None,
                    request_id: RequestId::new(),
                    pairing_code: agent.welcome.pairing_code.clone(),
                    permission_mode: PermissionMode::ApprovalRequired,
                },
            )
            .await;
        assert!(response.connection.is_some());
    }
    while ae.try_recv().is_ok() {}
    while he.try_recv().is_ok() {}
    while ie.try_recv().is_ok() {}
    let stream = RequestId::new();
    let event = |action| DesktopEvent {
        session,
        stream,
        action,
    };
    let hs = controller_sender(&relay, human).await;
    let ais = controller_sender(&relay, ai).await;
    relay
        .desktop_controller(ai, ig, event(Action::Open), &ais)
        .await;
    assert!(matches!(
        ie.try_recv(),
        Ok(WireMessage::Desktop(DesktopEvent {
            action: Action::Closed,
            ..
        }))
    ));
    relay
        .desktop_controller(human, hg, event(Action::Open), &hs)
        .await;
    let ticket = |m: WireMessage| match m {
        WireMessage::Desktop(DesktopEvent {
            action: Action::Attach(t),
            ..
        }) => t,
        _ => panic!("未返回媒体票据"),
    };
    let a = ticket(ae.try_recv().unwrap());
    let v = ticket(he.try_recv().unwrap());
    assert_ne!(a, v);
    let (mut source, wire) = tokio::io::duplex(2048);
    let (ar, aw) = tokio::io::split(wire);
    let (mut sink, wire) = tokio::io::duplex(2048);
    let (vr, vw) = tokio::io::split(wire);
    let agent_side = {
        let relay = relay.clone();
        tokio::spawn(async move { relay.attach_desktop_media(a, ar, aw).await })
    };
    let viewer_side = {
        let relay = relay.clone();
        let v = v.clone();
        tokio::spawn(async move { relay.attach_desktop_media(v, vr, vw).await })
    };
    write_video(&mut source, b"frame").await.unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), read_video(&mut sink))
            .await
            .unwrap()
            .unwrap(),
        b"frame"
    );
    assert!(
        relay
            .attach_desktop_media(v, tokio::io::empty(), tokio::io::sink())
            .await
            .is_err()
    );
    let key = Action::Input {
        epoch: 2,
        event: Input::Key { vk: 65, down: true },
    };
    relay
        .desktop_controller(human, hg, event(key.clone()), &hs)
        .await;
    assert!(ae.try_recv().is_err());
    relay
        .desktop_agent(
            aid,
            ag,
            event(Action::Geometry {
                epoch: 1,
                width: 1920,
                height: 1080,
                lan: true,
                clipboard: true,
                mini_rtc: true,
                crossdesk: true,
            }),
        )
        .await;
    assert!(matches!(he.try_recv(), Ok(WireMessage::Desktop(e)) if matches!(e.action, Action::Geometry { mini_rtc: true, .. })));
    relay.desktop_controller(human, hg, event(Action::CrossDeskRequest), &hs).await;
    assert!(ae.try_recv().is_err(), "旧 Agent 收不到新协议消息");
    relay.state.lock().await.agents.get_mut(&aid).unwrap().hello.environment.tools.push(
        remoteops_domain::ToolProfile { name: "remoteops-crossdesk-v1".into(), available: true, version: None }
    );
    relay.desktop_controller(ai, ig, event(Action::CrossDeskRequest), &ais).await;
    assert!(ae.try_recv().is_err(), "AI 绑定不能领取运维端的观看票据");
    relay.desktop_controller(human, hg, event(Action::CrossDeskRequest), &hs).await;
    let read_grant = |message| match message {
        WireMessage::Desktop(DesktopEvent { session: parent, stream: id, action: Action::CrossDeskGrant(ticket) })
            if parent == session && id == stream => ticket,
        _ => panic!("缺少本流 CrossDesk 观看票据"),
    };
    let host_ticket = read_grant(ae.try_recv().unwrap());
    let viewer_ticket = read_grant(he.try_recv().unwrap());
    assert_eq!(host_ticket, viewer_ticket);
    // 隔离集成运行使用真实 Relay 发出的两份票据，通过匿名管道交给原生 RTC 对端。
    if native {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let fixture = std::env::var_os("REMOTEOPS_CROSSDESK_RTC_FIXTURE").expect("需要原生测试程序");
        let port: u16 = std::env::var("REMOTEOPS_CROSSDESK_TEST_PORT").unwrap().parse().unwrap();
        let mut peer = tokio::process::Command::new(fixture)
            .arg(port.to_string())
            .current_dir(std::env::var_os("REMOTEOPS_CROSSDESK_LAB_DIRECTORY").unwrap())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let mut output = BufReader::new(peer.stdout.take().unwrap()).lines();
        let mut input = peer.stdin.take().unwrap();
        let mut bytes = serde_json::to_vec(&serde_json::json!({"host":host_ticket,"viewer":viewer_ticket})).unwrap();
        bytes.push(b'\n');
        input.write_all(&bytes).await.unwrap();
        let end = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut stage = 0;
        let controller_message = |enabled, epoch| Action::Control { enabled, epoch };
        loop {
            tokio::select! {
                line = output.next_line() => {
                    let Some(line) = line.unwrap() else { break };
                    match (stage, line.as_str()) {
                        (0, "take") | (2, "release") => {
                            let enable = stage == 0;
                            let action = controller_message(enable, if enable { 1 } else { 2 });
                            relay.desktop_controller(human, hg, event(action.clone()), &hs).await;
                            assert!(std::iter::from_fn(|| ae.try_recv().ok()).any(|message|
                                matches!(message, WireMessage::Desktop(e) if e.action == action)));
                            assert!(relay.state.lock().await.session_bindings[&session].desktop_takeover);
                            input.write_all(if enable { b"E" } else { b"O" }).await.unwrap();
                        }
                        (1, "taken") | (3, "released") => {
                            let enable = stage == 1;
                            let action = controller_message(enable, if enable { 2 } else { 3 });
                            relay.desktop_agent(aid, ag, event(action.clone())).await;
                            assert!(std::iter::from_fn(|| he.try_recv().ok()).any(|message|
                                matches!(message, WireMessage::Desktop(e) if e.action == action)));
                            assert_eq!(relay.state.lock().await.session_bindings[&session].desktop_takeover, enable);
                            input.write_all(b"A").await.unwrap();
                        }
                        (4, "close") => {
                            relay.desktop_controller(human, hg, event(Action::Closed), &hs).await;
                            assert!(std::iter::from_fn(|| ae.try_recv().ok()).any(|message|
                                matches!(message, WireMessage::Desktop(e) if e.action == Action::Closed)));
                            input.write_all(b"X").await.unwrap();
                        }
                        (5, "done") => {},
                        _ => panic!("原生测试程序与 Relay 的交接顺序不符"),
                    }
                    stage += 1;
                }
                () = tokio::time::sleep_until(end) => panic!("原生 RTC 验收超时"),
                () = tokio::time::sleep(std::time::Duration::from_millis(500)), if stage < 5 => {
                    relay.desktop_agent(aid, ag, event(Action::Heartbeat)).await;
                    relay.desktop_controller(human, hg, event(Action::Heartbeat), &hs).await;
                    assert!(std::iter::from_fn(|| ae.try_recv().ok()).any(|m| matches!(m, WireMessage::Desktop(e) if e.action == Action::Heartbeat)));
                    assert!(std::iter::from_fn(|| he.try_recv().ok()).any(|m| matches!(m, WireMessage::Desktop(e) if e.action == Action::Heartbeat)));
                    // 只传递已经通过 Relay 双向绑定校验的续租。RTC 流量本身不能续租。
                    input.write_all(b"H").await.expect("原生 RTC 对端已停止");
                }
            }
        }
        assert!(peer.wait().await.unwrap().success(), "原生 RTC 验收失败");
        assert_eq!(stage, 6);
        assert!(agent_side.await.unwrap().is_ok() && viewer_side.await.unwrap().is_ok());
        return;
    }
    relay.desktop_controller(human, hg, event(Action::CrossDeskRequest), &hs).await;
    assert!(ae.try_recv().is_err() && he.try_recv().is_err());
    assert!(!relay.state.lock().await.session_bindings[&session].desktop_takeover);
    relay.desktop_controller(human, hg, event(Action::RtcRequest), &hs).await;
    assert!(ae.try_recv().is_err() && he.try_recv().is_err(), "同一时间只能协商一种媒体");
    relay.desktop_controller(human, hg, event(Action::CrossDeskStop), &hs).await;
    assert!(matches!(ae.try_recv(), Ok(WireMessage::Desktop(e)) if e.action == Action::CrossDeskStop));
    relay.desktop_controller(ai, ig, event(Action::RtcRequest), &ais).await;
    assert!(ae.try_recv().is_err());
    // 自动模式先发私网请求；现场没有返回端点时，同一流仍应能取得 TURN 授权。
    let lan = Action::LanRequest { reverse:vec![], addresses: vec!["192.168.1.10".parse().unwrap()], certificate: vec![1;256] };
    relay.desktop_controller(ai,ig,event(lan.clone()),&ais).await;
    assert!(ae.try_recv().is_err(), "AI 不能发起人工桌面直连");
    relay.desktop_controller(human,hg,event(lan.clone()),&hs).await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==lan));
    relay.desktop_controller(human, hg, event(Action::RtcRequest), &hs).await;
    let grant = |m| match m {
        WireMessage::Desktop(DesktopEvent { action: Action::RtcGranted(g), .. }) => g,
        _ => panic!("缺少临时 TURN 凭据"),
    };
    let agent_grant = grant(ae.try_recv().unwrap());
    let viewer_grant = grant(he.try_recv().unwrap());
    assert_ne!(agent_grant.username, viewer_grant.username);
    assert!(agent_grant.expires > chrono::Utc::now().timestamp());
    relay.desktop_controller(human, hg, event(Action::RtcRequest), &hs).await;
    assert!(ae.try_recv().is_err());
    let signal = Action::RtcSignal(remoteops_protocol::desktop::rtc::Signal("test".into()));
    relay.desktop_controller(ai, ig, event(signal.clone()), &ais).await;
    assert!(ae.try_recv().is_err());
    relay.desktop_controller(human, hg, event(signal.clone()), &hs).await;
    assert!(matches!(ae.try_recv(), Ok(WireMessage::Desktop(e)) if e.action == signal));
    relay.desktop_controller(human, hg, event(Action::RtcStop), &hs).await;
    assert!(matches!(ae.try_recv(), Ok(WireMessage::Desktop(e)) if e.action == Action::RtcStop));
    relay.desktop_controller(human, hg, event(signal), &hs).await;
    assert!(ae.try_recv().is_err());
    let channel = RequestId::new();
    let clip_on=Action::ClipboardShare{epoch:2,channel,enabled:true};
    relay.desktop_controller(human,hg,event(clip_on.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "观察期间不得开启剪贴板");
    relay.desktop_controller(human,hg,event(lan.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "直连只协商一次");
    let offer=Action::LanOffer { reverse:None, address:"192.168.1.11:55443".parse().unwrap(),certificate:vec![2;256]};
    relay.desktop_agent(aid,ag,event(offer)).await;
    relay.desktop_controller(human,hg,event(Action::LanPermit{epoch:2}),&hs).await;
    assert!(ae.try_recv().is_err(), "控制端不能向自身授予直连输入");
    relay
        .desktop_controller(
            human,
            hg,
            event(Action::Control {
                enabled: true,
                epoch: 1,
            }),
            &hs,
        )
        .await;
    assert_eq!(
        relay.state.lock().await.session_bindings[&session].permission_mode_for(ControllerKind::Ai),
        PermissionMode::ReadOnly
    );
    while ae.try_recv().is_ok() {}
    relay
        .desktop_controller(human, hg, event(key.clone()), &hs)
        .await;
    assert!(ae.try_recv().is_err(), "现场尚未同意");
    relay
        .desktop_agent(
            aid,
            ag,
            event(Action::Control {
                enabled: true,
                epoch: 2,
            }),
        )
        .await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==Action::LanPermit {epoch:2}));
    relay
        .desktop_controller(ai, ig, event(key.clone()), &ais)
        .await;
    relay
        .desktop_controller(human, hg + 1, event(key.clone()), &hs)
        .await;
    assert!(ae.try_recv().is_err());
    relay
        .desktop_controller(human, hg, event(key.clone()), &hs)
        .await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==key));
    relay.desktop_controller(ai,ig,event(clip_on.clone()),&ais).await;
    assert!(ae.try_recv().is_err());
    relay.desktop_controller(human,hg,event(clip_on.clone()),&hs).await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==clip_on));
    let packet=remoteops_protocol::desktop::clipboard::Packet::encode(b"fixture").unwrap();
    let clip_data=Action::ClipboardData{epoch:2,channel,packet:packet.clone()};
    relay.desktop_controller(human,hg,event(clip_data.clone()),&hs).await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==clip_data));
    let clip_off=Action::ClipboardShare{epoch:2,channel,enabled:false};
    relay.desktop_controller(human,hg,event(clip_off.clone()),&hs).await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==clip_off));
    relay.desktop_controller(human,hg,event(clip_data.clone()),&hs).await;
    assert!(ae.try_recv().is_err());
    let switch = |enabled,epoch| event(Action::Control { enabled,epoch });
    relay.desktop_controller(human,hg,switch(false,2),&hs).await;
    assert!(relay.state.lock().await.session_bindings[&session].desktop_takeover,
        "现场释放键鼠之前，AI 继续只读");
    while ae.try_recv().is_ok() {}
    relay.desktop_agent(aid,ag,switch(false,2)).await;
    relay.desktop_agent(aid,ag,switch(true,2)).await;
    relay.desktop_controller(human,hg,event(key.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "交还期间旧回执和人工输入均失效");
    assert!(relay.state.lock().await.session_bindings[&session].desktop_takeover);
    relay.desktop_agent(aid,ag,switch(false,3)).await;
    while ae.try_recv().is_ok() {}
    relay.desktop_controller(human,hg,event(key.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "观察模式拒绝旧按键");
    assert!(!relay.state.lock().await.session_bindings[&session].desktop_takeover);
    write_video(&mut source,b"observing").await.unwrap();
    assert_eq!(read_video(&mut sink).await.unwrap(),b"observing");
    // 人工交还后的第一次写操作需要重新观察；截图请求失败或观察到锁屏都不解锁。
    let target = remoteops_domain::VisualTarget::Control {
        window_fingerprint: "before-takeover".into(), target_fingerprint: "old-target".into(),
        automation_id: None, name: None, control_type: None,
    };
    let click = || test_remote_request(session, RemoteOperation::VisualInvoke {
        target: target.clone(), action: "invoke".into(),
    });
    for ready in [false, true] {
        while ie.try_recv().is_ok() {}
        relay.forward_controller_request(ai, ig, click(), &ais).await;
        let Ok(WireMessage::Error { code, .. }) = ie.try_recv() else {panic!("缺少图形操作拒绝回执")};
        assert_eq!(code, "fresh_visual_observation_required");
        let observe = test_remote_request(session, RemoteOperation::VisualObserve {
            include_screenshot: false, include_ui_tree: true,
        });
        let request_id = observe.request_id;
        relay.forward_controller_request(ai, ig, observe, &ais).await;
        assert!(relay.state.lock().await.in_flight.contains_key(&request_id));
        relay.forward_agent_message(aid, ag, WireMessage::RemoteResponse(remoteops_protocol::RemoteResponse {
            request_id, session_id: session, exit_code: None, summary: String::new(),
            error_code: None, payload_base64: None, sha256: None,
            details: Some(serde_json::json!({"state": if ready {"ready"} else {"no_interactive_desktop"},
                "ui_tree":{"available":ready}})),
        })).await;
    }
    assert!(matches!(relay.state.lock().await.session_bindings[&session].visual_handoff, VisualHandoff::Ready));
    while ae.try_recv().is_ok() {}
    while he.try_recv().is_ok() {}

    relay.state.lock().await.session_bindings.get_mut(&session).unwrap().agent_permission_mode=PermissionMode::ReadOnly;
    relay.desktop_controller(human,hg,switch(true,3),&hs).await;
    assert!(ae.try_recv().is_err(), "只读授权不能切换控制");
    assert!(!relay.state.lock().await.session_bindings[&session].desktop_takeover);
    relay.state.lock().await.session_bindings.get_mut(&session).unwrap().agent_permission_mode=PermissionMode::ApprovalRequired;
    relay.desktop_controller(human,hg,switch(true,3),&hs).await;
    relay.desktop_agent(aid,ag,switch(true,4)).await;
    while ae.try_recv().is_ok() {}
    relay.desktop_controller(human,hg,event(key.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "再次控制仍拒绝上次的键鼠代次");
    let fresh_key=Action::Input{epoch:4,event:Input::Key{vk:65,down:false}};
    relay.desktop_controller(human,hg,event(fresh_key.clone()),&hs).await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==fresh_key));
    relay
        .desktop_controller(human, hg, event(Action::Closed), &hs)
        .await;
    relay.desktop_controller(human, hg, event(key), &hs).await;
    assert!(!relay.state.lock().await.session_bindings[&session].desktop_takeover);
    for handle in [agent_side, viewer_side] {
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert!(read_video(&mut sink).await.is_err());
    let fresh = RequestId::new();
    relay
        .desktop_controller(
            human,
            hg,
            DesktopEvent {
                session,
                stream: fresh,
                action: Action::Open,
            },
            &hs,
        )
        .await;
    relay
        .desktop_controller(human, hg, event(Action::Closed), &hs)
        .await;
    assert!(relay.state.lock().await.desktops.contains_key(&session));
    tokio::time::sleep(remoteops_protocol::desktop::LEASE).await;
    let mut state = relay.state.lock().await;
    relay.prune_desktops(&mut state);
    assert!(!state.desktops.contains_key(&session));
}
