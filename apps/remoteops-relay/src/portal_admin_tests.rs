#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "真实 HTTP 顺序覆盖管理授权、资产迁移、重试和请求校验"
)]
async fn actual_http_portal_admin_acl_transfer_and_idempotency() {
    let owner = ControllerOwnerId::new();
    let fixture = HttpFixture::new(owner).await;
    let mut principals = Vec::new();
    for (name, principal_owner, assets) in [
        ("ops-a", owner, vec!["asset-w"]),
        ("ops-b", owner, vec!["asset-l"]),
    ] {
        let request = serde_json::json!({
            "name": name,
            "owner_id": principal_owner,
            "allowed_assets": assets,
        });
        let (status, created) = fixture
            .request(
                "POST",
                "/api/admin/portal/principals",
                Some("test-admin"),
                &request.to_string(),
            )
            .await;
        assert_eq!(status, 200);
        principals.push(created);
    }
    let principal_a = principals[0]["principal"]["principal_id"].as_str().unwrap();
    let principal_b = principals[1]["principal"]["principal_id"].as_str().unwrap();
    // 公共管理入口拒绝创建外部 owner，迁移的预期归属另作冲突验证。
    let (status, rejected_owner) = fixture
        .request(
            "POST",
            "/api/admin/portal/principals",
            Some("test-admin"),
            &serde_json::json!({
                "name": "other-owner",
                "owner_id": ControllerOwnerId::new(),
                "allowed_assets": ["asset-l"],
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, 400);
    assert!(rejected_owner.get("token").is_none());
    let token_a = principals[0]["token"].as_str().unwrap();
    let token_b = principals[1]["token"].as_str().unwrap();
    let (status, created_asset) = fixture
        .request(
            "POST",
            "/api/admin/portal/assets",
            Some("test-admin"),
            &serde_json::json!({"asset_id":"asset-l","principal_id":principal_b}).to_string(),
        )
        .await;
    assert_eq!(status, 200);
    let target = created_asset["asset"]["target_id"].as_str().unwrap();
    let principal_path = format!("/api/admin/portal/principals/{principal_a}");
    let acl_path = format!("{principal_path}/assets");
    let asset_path = format!("/api/admin/portal/assets/{target}");
    let transfer_path = format!("{asset_path}/transfer");
    let operation_path = format!("/api/admin/portal/operations/{}", Uuid::new_v4());
    let acl_request = serde_json::json!({
        "expected_revision": 1,
        "idempotency_key": Uuid::new_v4(),
        "add": ["asset-l"],
        "remove": [],
    });
    let transfer_request = serde_json::json!({
        "expected_principal_id": principal_b,
        "destination_principal_id": principal_a,
        "expected_revision": created_asset["asset"]["revision"],
        "idempotency_key": Uuid::new_v4(),
    });

    // 普通门户凭据不能读取管理快照，也不能修改白名单或迁移归属。
    for (method, path, body) in [
        ("GET", principal_path.as_str(), String::new()),
        ("GET", asset_path.as_str(), String::new()),
        ("GET", operation_path.as_str(), String::new()),
        ("GET", "/api/admin/portal/audit?limit=20", String::new()),
        ("POST", acl_path.as_str(), acl_request.to_string()),
        ("POST", transfer_path.as_str(), transfer_request.to_string()),
    ] {
        let (status, _) = fixture.request(method, path, Some(token_a), &body).await;
        assert_eq!(status, 401, "{method} {path}");
    }
    let (status, principal_before) = fixture
        .request("GET", &principal_path, Some("test-admin"), "")
        .await;
    assert_eq!(status, 200);
    assert_eq!(principal_before["revision"], 1);
    let (status, asset_before) = fixture
        .request("GET", &asset_path, Some("test-admin"), "")
        .await;
    assert_eq!(status, 200);
    assert_eq!(asset_before["target_id"], target);
    assert_eq!(asset_before["principal_id"], principal_b);
    assert!(asset_before["agent_id"].is_null());

    let (status, acl_operation) = fixture
        .request(
            "POST",
            &acl_path,
            Some("test-admin"),
            &acl_request.to_string(),
        )
        .await;
    assert_eq!(status, 200, "{acl_operation}");
    assert_eq!(acl_operation["state"], "completed");
    assert_eq!(acl_operation["previous_principal"]["revision"], 1);
    assert_eq!(acl_operation["principal"]["revision"], 2);
    let (status, acl_retry) = fixture
        .request(
            "POST",
            &acl_path,
            Some("test-admin"),
            &acl_request.to_string(),
        )
        .await;
    assert_eq!(status, 200, "{acl_retry}");
    assert_eq!(acl_retry, acl_operation);
    let mut conflicting_acl = acl_request.clone();
    conflicting_acl["add"] = serde_json::json!(["asset-unrelated"]);
    let (status, conflict) = fixture
        .request(
            "POST",
            &acl_path,
            Some("test-admin"),
            &conflicting_acl.to_string(),
        )
        .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["code"], "idempotency_conflict");

    let (status, transfer_operation) = fixture
        .request(
            "POST",
            &transfer_path,
            Some("test-admin"),
            &transfer_request.to_string(),
        )
        .await;
    assert_eq!(status, 200, "{transfer_operation}");
    assert_eq!(transfer_operation["state"], "completed");
    assert_eq!(transfer_operation["previous_asset"], asset_before);
    assert_eq!(transfer_operation["asset"]["target_id"], target);
    assert_eq!(transfer_operation["asset"]["principal_id"], principal_a);
    assert_eq!(
        transfer_operation["asset"]["revision"].as_u64().unwrap(),
        asset_before["revision"].as_u64().unwrap() + 1
    );
    let (status, transfer_retry) = fixture
        .request(
            "POST",
            &transfer_path,
            Some("test-admin"),
            &transfer_request.to_string(),
        )
        .await;
    assert_eq!(status, 200, "{transfer_retry}");
    assert_eq!(transfer_retry, transfer_operation);

    // 原门户凭据继续有效，归属变化后只有目标主体能查询迁入资产。
    let portal_asset_path = "/api/portal/v1/assets/asset-l/remote-access";
    let (status, state) = fixture
        .request("GET", portal_asset_path, Some(token_a), "")
        .await;
    assert_eq!(status, 200, "{state}");
    let (status, _) = fixture
        .request("GET", portal_asset_path, Some(token_b), "")
        .await;
    assert!(matches!(status, 403 | 404));
    let read_operation_path = format!(
        "/api/admin/portal/operations/{}",
        transfer_operation["operation_id"].as_str().unwrap()
    );
    let (status, recorded_operation) = fixture
        .request("GET", &read_operation_path, Some("test-admin"), "")
        .await;
    assert_eq!(status, 200);
    assert_eq!(recorded_operation, transfer_operation);
    let (status, audit) = fixture
        .request(
            "GET",
            "/api/admin/portal/audit?limit=20",
            Some("test-admin"),
            "",
        )
        .await;
    assert_eq!(status, 200, "{audit}");
    let events = audit.as_array().unwrap();
    assert!(events.iter().any(|event| {
        event["operation_id"] == acl_operation["operation_id"]
            && event["previous_principal"]["revision"] == 1
            && event["principal"]["revision"] == 2
    }));
    assert!(events.iter().any(|event| {
        event["operation_id"] == transfer_operation["operation_id"]
            && event["previous_asset"]["principal_id"] == principal_b
            && event["asset"]["principal_id"] == principal_a
    }));
    let audit_json = audit.to_string();
    for secret in [
        token_a,
        token_b,
        created_asset["enrollment_token"].as_str().unwrap(),
    ] {
        assert!(!audit_json.contains(secret));
    }
    assert!(!audit_json.contains("\"token\""));
    assert!(!audit_json.contains("\"enrollment_token\""));

    let mut stale_acl = acl_request.clone();
    stale_acl["idempotency_key"] = serde_json::json!(Uuid::new_v4());
    let (status, conflict) = fixture
        .request(
            "POST",
            &acl_path,
            Some("test-admin"),
            &stale_acl.to_string(),
        )
        .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["code"], "revision_conflict");
    for path in [&acl_path, &transfer_path] {
        let valid = if path == &acl_path {
            &acl_request
        } else {
            &transfer_request
        };
        let mut unknown = valid.clone();
        unknown["unexpected"] = serde_json::json!(true);
        let mut missing_key = valid.clone();
        missing_key
            .as_object_mut()
            .unwrap()
            .remove("idempotency_key");
        for invalid in [unknown, missing_key] {
            let (status, _) = fixture
                .request("POST", path, Some("test-admin"), &invalid.to_string())
                .await;
            assert_eq!(status, 400, "{path}");
        }
    }
    let wrong_owner = serde_json::json!({
        "expected_principal_id": principal_b,
        "destination_principal_id": principal_a,
        "expected_revision": transfer_operation["asset"]["revision"],
        "idempotency_key": Uuid::new_v4(),
    });
    let (status, conflict) = fixture
        .request(
            "POST",
            &transfer_path,
            Some("test-admin"),
            &wrong_owner.to_string(),
        )
        .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["code"], "ownership_conflict");
    fixture.shutdown().await;
}
