#[tokio::test]
async fn paired_desktop_is_human_only_and_bound_to_the_live_stream() {
    use remoteops_protocol::desktop::{Action, DesktopEvent, Input, read_video, write_video};
    let relay = std::sync::Arc::new(relay(Duration::minutes(10)));
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
            }),
        )
        .await;
    let lan = Action::LanRequest { addresses: vec!["192.168.1.10".parse().unwrap()], certificate: vec![1;256] };
    relay.desktop_controller(ai,ig,event(lan.clone()),&ais).await;
    assert!(ae.try_recv().is_err(), "AI 不能发起人工桌面直连");
    let channel = RequestId::new();
    let clip_on=Action::ClipboardShare{epoch:2,channel,enabled:true};
    relay.desktop_controller(human,hg,event(clip_on.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "观察期间不得开启剪贴板");
    relay.desktop_controller(human,hg,event(lan.clone()),&hs).await;
    assert!(matches!(ae.try_recv(),Ok(WireMessage::Desktop(e)) if e.action==lan));
    relay.desktop_controller(human,hg,event(lan.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "直连只协商一次");
    let offer=Action::LanOffer { address:"192.168.1.11:55443".parse().unwrap(),certificate:vec![2;256]};
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
    relay.desktop_agent(aid,ag,switch(false,3)).await;
    while ae.try_recv().is_ok() {}
    relay.desktop_controller(human,hg,event(key.clone()),&hs).await;
    assert!(ae.try_recv().is_err(), "观察模式拒绝旧按键");
    assert!(!relay.state.lock().await.session_bindings[&session].desktop_takeover);
    write_video(&mut source,b"observing").await.unwrap();
    assert_eq!(read_video(&mut sink).await.unwrap(),b"observing");

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
