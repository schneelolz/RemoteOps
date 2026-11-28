use super::*;
fn description() -> HostServiceDescription {
    HostServiceDescription {
        service_id: "grclient".into(),
        service_version: "1".into(),
        instance_id: "test".into(),
        protocol_version: 1,
        capabilities: vec![],
    }
}
fn query() -> HostQuery {
    HostQuery {
        request_id: remoteops_domain::RequestId::new(),
        session_id: remoteops_domain::SessionId::new(),
        instance_id: "test".into(),
        capability_id: "user".into(),
        version: "1".into(),
        arguments: json!({}),
    }
}
#[tokio::test]
async fn reverse_request_cancel_removes_waiter_and_notifies_host() {
    let (output, mut receiver) = mpsc::channel(32);
    let provider = Arc::new(Provider {
        description: description(),
        output,
        pending: Arc::default(),
        sequence: AtomicU64::new(1),
    });
    let worker = provider.clone();
    let task = tokio::spawn(async move { worker.query(query()).await });
    assert_eq!(
        receiver.recv().await.unwrap().method.as_deref(),
        Some("host.invoke")
    );
    task.abort();
    let _ = task.await;
    assert!(provider.pending.lock().unwrap().is_empty());
    assert_eq!(
        receiver.recv().await.unwrap().method.as_deref(),
        Some("$/cancelRequest")
    );
}
#[tokio::test]
async fn reverse_concurrency_limit_rejects_without_replay() {
    let (output, _receiver) = mpsc::channel(32);
    let provider = Provider {
        description: description(),
        output,
        pending: Arc::default(),
        sequence: AtomicU64::new(1),
    };
    let mut receivers = vec![];
    for number in 0..MAX_CONCURRENT {
        let (sender, receiver) = oneshot::channel();
        provider
            .pending
            .lock()
            .unwrap()
            .insert(format!("occupied-{number}"), sender);
        receivers.push(receiver);
    }
    assert!(
        provider
            .query(query())
            .await
            .unwrap_err()
            .to_string()
            .contains("host_busy")
    );
    assert_eq!(provider.pending.lock().unwrap().len(), MAX_CONCURRENT);
}
#[test]
fn invalid_instance_and_service_visual_are_rejected() {
    let mut input: Initialize = serde_json::from_value(json!({"protocolVersion":1,"role":"udserver","instanceId":"../bad","relay":"127.0.0.1:1","serverName":"localhost","allowedCapabilities":[]})).unwrap();
    assert!(configure(&input).is_err());
    input.instance_id = "safe".into();
    input.visual_enabled = true;
    assert!(configure(&input).is_err());
}
