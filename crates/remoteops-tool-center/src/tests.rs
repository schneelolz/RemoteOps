use std::io::Write as _;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::*;

fn request(revision: u64, command: ToolCenterCommand) -> ToolCenterRequest {
    ToolCenterRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        expected_revision: revision,
        command,
    }
}

async fn run(center: &ToolCenter, command: ToolCenterCommand) -> ToolCenterResponse {
    center
        .execute(
            "admin:test",
            request(center.snapshot().unwrap().revision, command),
        )
        .await
}

#[tokio::test]
async fn global_guide_is_revisioned_idempotent_bounded_and_persistent() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    assert_eq!(center.snapshot().unwrap().guide_markdown, "");
    let change = request(
        0,
        ToolCenterCommand::SetGuide {
            guide_markdown: "# 工具选择\nA 优先，结果未知不重放".into(),
        },
    );
    let response = center.execute("admin:test", change.clone()).await;
    assert!(response.error.is_none());
    assert_eq!(response.revision, 1);
    assert_eq!(center.execute("admin:test", change).await, response);
    assert_eq!(
        center.catalog().unwrap().guide_markdown,
        response.data["guide_markdown"]
    );
    let conflict = center
        .execute(
            "admin:test",
            request(
                0,
                ToolCenterCommand::SetGuide {
                    guide_markdown: "stale".into(),
                },
            ),
        )
        .await;
    assert_eq!(conflict.error.unwrap().code, "revision_conflict");
    for value in ["中".repeat(10923), "bad\0guide".into()] {
        assert_eq!(
            run(
                &center,
                ToolCenterCommand::SetGuide {
                    guide_markdown: value
                }
            )
            .await
            .error
            .unwrap()
            .code,
            "invalid_guide"
        );
    }
    let maximum = "a".repeat(32768);
    let saved = run(
        &center,
        ToolCenterCommand::SetGuide {
            guide_markdown: maximum.clone(),
        },
    )
    .await;
    assert!(saved.error.is_none());
    let repeated = run(
        &center,
        ToolCenterCommand::SetGuide {
            guide_markdown: maximum.clone(),
        },
    )
    .await;
    assert_eq!(saved.revision, repeated.revision);
    drop(center);
    let reopened = ToolCenter::open(root.path()).unwrap();
    assert_eq!(reopened.snapshot().unwrap().guide_markdown, maximum);
    assert_eq!(reopened.snapshot().unwrap().audit[0].action, "set_guide");
}

#[tokio::test]
async fn schema_one_migration_preserves_tools_packages_audit_and_requests() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let receipt = upload(&center, &archive("tool.exe", b"fixture")).await;
    let registered = run(
        &center,
        ToolCenterCommand::PutTool {
            tool: managed(&receipt),
        },
    )
    .await;
    assert!(registered.error.is_none());
    let old_snapshot = center.snapshot().unwrap();
    drop(center);
    let connection = rusqlite::Connection::open(root.path().join("tools.sqlite3")).unwrap();
    connection
        .execute_batch("ALTER TABLE policy DROP COLUMN guide_markdown; PRAGMA user_version=1;")
        .unwrap();
    let old_requests: i64 = connection
        .query_row("SELECT count(*) FROM requests", [], |row| row.get(0))
        .unwrap();
    drop(connection);
    let migrated = ToolCenter::open(root.path()).unwrap();
    assert_eq!(migrated.snapshot().unwrap(), old_snapshot);
    assert!(migrated.package_path(&receipt.sha256).unwrap().exists());
    let connection = rusqlite::Connection::open(root.path().join("tools.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM requests", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        old_requests
    );
}

fn archive(name: &str, value: &[u8]) -> Vec<u8> {
    let mut file = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    file.start_file(
        name,
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    file.write_all(value).unwrap();
    file.finish().unwrap().into_inner()
}

async fn upload(center: &ToolCenter, bytes: &[u8]) -> PackageReceipt {
    let hash = format!("{:x}", Sha256::digest(bytes));
    let begun = run(
        center,
        ToolCenterCommand::BeginUpload {
            size: bytes.len() as u64,
            sha256: hash,
        },
    )
    .await;
    assert!(begun.error.is_none(), "{:?}", begun.error);
    let id = begun.data["upload_id"].as_str().unwrap().to_string();
    for (index, chunk) in bytes.chunks(PACKAGE_CHUNK_SIZE).enumerate() {
        let response = run(
            center,
            ToolCenterCommand::UploadChunk {
                upload_id: id.clone(),
                offset: (index * PACKAGE_CHUNK_SIZE) as u64,
                data_base64: STANDARD.encode(chunk),
            },
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
    }
    let response = run(center, ToolCenterCommand::FinishUpload { upload_id: id }).await;
    assert!(response.error.is_none(), "{:?}", response.error);
    serde_json::from_value(response.data).unwrap()
}

fn managed(receipt: &PackageReceipt) -> ManagedTool {
    let mut tool: Value = serde_json::from_slice(include_bytes!(
        "../../../examples/resource-site/remoteops/v1/index.json"
    ))
    .unwrap();
    let mut descriptor: remoteops_resources::ResourceTool =
        serde_json::from_value(tool["tools"].as_array_mut().unwrap().remove(0)).unwrap();
    descriptor.package.size = receipt.size;
    descriptor.package.sha256 = receipt.sha256.clone();
    descriptor.id = "example".into();
    descriptor.version = "1.0.0".into();
    ManagedTool {
        id: "example".into(),
        name: "测试工具".into(),
        purpose: "验证集中目录".into(),
        instructions: "按操作定义调用".into(),
        examples: vec![],
        dependencies: vec![],
        enabled: true,
        versions: vec![ToolVersion {
            version: "1.0.0".into(),
            packages: vec![ToolPackage {
                tool: descriptor,
                source: receipt.source.clone(),
                file_verified: true,
                tests: vec![],
            }],
        }],
    }
}

#[tokio::test]
async fn upload_register_restart_and_enable_are_separate_facts() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    assert!(!center.snapshot().unwrap().enabled);
    let bytes = archive("node/node.exe", b"test native payload");
    let receipt = upload(&center, &bytes).await;
    assert!(receipt.file_verified);
    assert_eq!(
        std::fs::read(center.package_path(&receipt.sha256).unwrap()).unwrap(),
        bytes
    );
    let saved = run(
        &center,
        ToolCenterCommand::PutTool {
            tool: managed(&receipt),
        },
    )
    .await;
    assert!(saved.error.is_none(), "{:?}", saved.error);
    let snapshot = center.snapshot().unwrap();
    assert!(!snapshot.tools[0].enabled);
    assert_eq!(snapshot.tools[0].versions[0].packages[0].tests.len(), 0);
    assert_eq!(
        snapshot.tools[0].versions[0].packages[0].tool.platforms[0].status,
        "pending"
    );
    assert_eq!(
        center.catalog().unwrap().tools,
        Vec::<remoteops_resources::ResourceTool>::new()
    );
    run(
        &center,
        ToolCenterCommand::SetToolEnabled {
            tool_id: "example".into(),
            enabled: true,
        },
    )
    .await;
    run(&center, ToolCenterCommand::SetPolicy { enabled: true }).await;
    assert_eq!(center.catalog().unwrap().tools.len(), 1);
    drop(center);
    let center = ToolCenter::open(root.path()).unwrap();
    assert!(center.snapshot().unwrap().enabled);
    assert_eq!(center.catalog().unwrap().tools.len(), 1);
    run(&center, ToolCenterCommand::SetPolicy { enabled: false }).await;
    assert_eq!(
        center.catalog().unwrap().tools,
        Vec::<remoteops_resources::ResourceTool>::new()
    );
}

#[tokio::test]
async fn revision_and_actor_scoped_request_ids_prevent_overwrite_and_replay() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let request = request(0, ToolCenterCommand::SetPolicy { enabled: true });
    let first = center.execute("one", request.clone()).await;
    let repeated = center.execute("one", request.clone()).await;
    assert_eq!(first, repeated);
    assert_eq!(center.snapshot().unwrap().revision, 1);
    let second_process = ToolCenter::open(root.path()).unwrap();
    let rejected = second_process.execute("two", request.clone()).await;
    assert_eq!(rejected.error.unwrap().code, "revision_conflict");
    let changed = ToolCenterRequest {
        command: ToolCenterCommand::SetPolicy { enabled: false },
        ..request
    };
    assert_eq!(
        center.execute("one", changed).await.error.unwrap().code,
        "request_id_conflict"
    );
    assert!(center.snapshot().unwrap().enabled);
}

#[tokio::test]
async fn chunks_are_bounded_ordered_idempotent_and_actor_isolated() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let bytes = archive("payload.exe", b"test");
    let begun = run(
        &center,
        ToolCenterCommand::BeginUpload {
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        },
    )
    .await;
    let id = begun.data["upload_id"].as_str().unwrap().to_string();
    let unauthorized = center
        .execute(
            "other",
            request(
                0,
                ToolCenterCommand::UploadChunk {
                    upload_id: id.clone(),
                    offset: 0,
                    data_base64: STANDARD.encode(&bytes),
                },
            ),
        )
        .await;
    assert_eq!(unauthorized.error.unwrap().code, "upload_not_found");
    let bad = run(
        &center,
        ToolCenterCommand::UploadChunk {
            upload_id: id.clone(),
            offset: 1,
            data_base64: STANDARD.encode(&bytes),
        },
    )
    .await;
    assert_eq!(bad.error.unwrap().code, "invalid_chunk");
    let large = run(
        &center,
        ToolCenterCommand::UploadChunk {
            upload_id: id.clone(),
            offset: 0,
            data_base64: STANDARD.encode(vec![0; PACKAGE_CHUNK_SIZE + 1]),
        },
    )
    .await;
    assert_eq!(large.error.unwrap().code, "chunk_limit");
    let chunk = request(
        0,
        ToolCenterCommand::UploadChunk {
            upload_id: id.clone(),
            offset: 0,
            data_base64: STANDARD.encode(&bytes),
        },
    );
    assert_eq!(
        center.execute("admin:test", chunk.clone()).await,
        center.execute("admin:test", chunk).await
    );
    assert!(
        run(&center, ToolCenterCommand::FinishUpload { upload_id: id })
            .await
            .error
            .is_none()
    );
}

#[tokio::test]
async fn damaged_zip_and_digest_mismatch_never_become_packages() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    for (bytes, hash, error_code) in [
        (
            b"damaged ZIP".to_vec(),
            format!("{:x}", Sha256::digest(b"damaged ZIP")),
            "invalid_zip",
        ),
        (
            archive("payload.exe", b"data"),
            "a".repeat(64),
            "hash_mismatch",
        ),
        (
            archive("../escape.exe", b"data"),
            String::new(),
            "unsafe_zip",
        ),
    ] {
        let hash = if hash.is_empty() {
            format!("{:x}", Sha256::digest(&bytes))
        } else {
            hash
        };
        let begun = run(
            &center,
            ToolCenterCommand::BeginUpload {
                size: bytes.len() as u64,
                sha256: hash.clone(),
            },
        )
        .await;
        let id = begun.data["upload_id"].as_str().unwrap().to_string();
        run(
            &center,
            ToolCenterCommand::UploadChunk {
                upload_id: id.clone(),
                offset: 0,
                data_base64: STANDARD.encode(&bytes),
            },
        )
        .await;
        assert_eq!(
            run(&center, ToolCenterCommand::FinishUpload { upload_id: id })
                .await
                .error
                .unwrap()
                .code,
            error_code
        );
        assert!(center.package_path(&hash).is_err());
    }
}

#[tokio::test]
async fn changed_content_requires_new_version_and_test_evidence_cannot_be_forged() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let first = upload(&center, &archive("payload.exe", b"first")).await;
    let mut tool = managed(&first);
    tool.versions[0].packages[0].tool.requires_desktop = true;
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool: tool.clone() })
            .await
            .error
            .is_none()
    );
    let mut record = PlatformTest {
        os: "windows".into(),
        arch: "x86_64".into(),
        environment: "Windows test build".into(),
        desktop: false,
        windows_build: Some(26100),
        status: "passed".into(),
        result: "headless only".into(),
        tested_at: chrono::Utc::now().to_rfc3339(),
        recorded_by: "forged".into(),
    };
    let tested = run(
        &center,
        ToolCenterCommand::RecordTest {
            tool_id: tool.id.clone(),
            version: "1.0.0".into(),
            sha256: first.sha256.clone(),
            record: record.clone(),
        },
    )
    .await;
    assert_eq!(tested.error.unwrap().code, "desktop_test_required");
    record.desktop = true;
    assert!(
        run(
            &center,
            ToolCenterCommand::RecordTest {
                tool_id: tool.id.clone(),
                version: "1.0.0".into(),
                sha256: first.sha256.clone(),
                record
            }
        )
        .await
        .error
        .is_none()
    );
    let snapshot = center.snapshot().unwrap();
    assert_eq!(
        snapshot.tools[0].versions[0].packages[0].tests[0].recorded_by,
        "admin:test"
    );
    assert_eq!(
        snapshot.tools[0].versions[0].packages[0].tool.platforms[0].status,
        "passed"
    );
    let second = upload(&center, &archive("payload.exe", b"second")).await;
    tool.versions[0].packages[0].tool.package.sha256 = second.sha256;
    tool.versions[0].packages[0].tool.package.size = second.size;
    assert_eq!(
        run(&center, ToolCenterCommand::PutTool { tool })
            .await
            .error
            .unwrap()
            .code,
        "immutable_tool_version"
    );
}

#[tokio::test]
async fn signed_external_urls_are_hidden_and_metadata_edit_preserves_signature() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let url = "https://oss.example/pkg.zip?Signature=secret-test-signature&Expires=999";
    let hash = "b".repeat(64);
    let source = PackageSource::External { url: url.into() };
    let key = format!("external:{:x}", Sha256::digest(url.as_bytes()));
    let connection = rusqlite::Connection::open(root.path().join("tools.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO packages VALUES(?1,?2,123,?3)",
            rusqlite::params![hash, key, serde_json::to_string(&source).unwrap()],
        )
        .unwrap();
    let receipt = PackageReceipt {
        size: 123,
        sha256: hash,
        file_verified: true,
        source,
    };
    let mut tool = managed(&receipt);
    tool.versions[0].packages[0].source = PackageSource::External {
        url: "https://oss.example/pkg.zip".into(),
    };
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool })
            .await
            .error
            .is_none()
    );
    let mut snapshot = center.snapshot().unwrap();
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("secret-test-signature")
    );
    assert!(!format!("{snapshot:?}").contains("secret-test-signature"));
    snapshot.tools[0].name = "更新名称".into();
    assert!(
        run(
            &center,
            ToolCenterCommand::PutTool {
                tool: snapshot.tools.remove(0)
            }
        )
        .await
        .error
        .is_none()
    );
    run(
        &center,
        ToolCenterCommand::SetToolEnabled {
            tool_id: "example".into(),
            enabled: true,
        },
    )
    .await;
    run(&center, ToolCenterCommand::SetPolicy { enabled: true }).await;
    assert_eq!(center.catalog().unwrap().tools[0].package.url, url);
    assert_eq!(json!(PackageSource::Hosted), json!({"kind":"hosted"}));
}

#[test]
fn external_url_policy_allows_signed_https_and_rejects_credentials_or_fragments() {
    assert!(packages::external_url("https://oss.example/pkg.zip?Signature=private").is_ok());
    for value in [
        "http://example/a.zip",
        "https://user:secret@example/a.zip",
        "https://example/a.zip#fragment",
        "not a url",
    ] {
        assert!(packages::external_url(value).is_err());
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn actual_https_external_import_checks_signatures_expiry_hash_and_zip_without_retention() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate = certified.cert.der().clone();
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let bytes = archive("tool.exe", b"not executed");
    let expected = format!("{:x}", Sha256::digest(&bytes));
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            let mut request = vec![0_u8; 16384];
            let count = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            let expired = request.contains("/expired");
            let bad_zip = request.contains("/bad.zip");
            let redirect = request.contains("/redirect");
            let body = if bad_zip {
                b"broken ZIP".as_slice()
            } else {
                bytes.as_slice()
            };
            let status = if expired {
                "403 Forbidden"
            } else if redirect {
                "302 Found"
            } else {
                "200 OK"
            };
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: https://other.example/secret\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            let _ = socket.write_all(body).await;
            let _ = socket.shutdown().await;
        }
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(reqwest::Certificate::from_der(certificate.as_ref()).unwrap())
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let url = format!("{origin}/tool.zip?Signature=private-test-signature");
    let receipt =
        packages::import_external_with_client(root.path(), &url, Some(&expected), &client)
            .await
            .unwrap();
    assert_eq!(receipt.sha256, expected);
    assert!(receipt.file_verified);
    assert!(!format!("{receipt:?}").contains("private-test-signature"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    for (path, hash, code) in [
        ("/tool.zip", Some("a".repeat(64)), "hash_mismatch"),
        (
            "/expired?Signature=private-test-signature",
            None,
            "external_http_status",
        ),
        ("/redirect", None, "external_http_status"),
        ("/bad.zip", None, "invalid_zip"),
    ] {
        let error = packages::import_external_with_client(
            root.path(),
            &format!("{origin}{path}"),
            hash.as_deref(),
            &client,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, code);
        assert!(!format!("{error:?}").contains("private-test-signature"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    // 使用实际 HTTPS 校验回执准备隔离仓库；测试 CA 只属于上面的下载客户端。
    let center = ToolCenter::open(root.path()).unwrap();
    let source_key = format!("external:{:x}", Sha256::digest(url.as_bytes()));
    rusqlite::Connection::open(root.path().join("tools.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO packages VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                receipt.sha256,
                source_key,
                i64::try_from(receipt.size).unwrap(),
                serde_json::to_string(&receipt.source).unwrap()
            ],
        )
        .unwrap();
    register_contract_example(
        &center,
        include_str!("../../../deploy/skills/remoteops/references/examples/put-tool-external.json"),
        &receipt,
    )
    .await;
    assert!(
        !serde_json::to_string(&center.snapshot().unwrap())
            .unwrap()
            .contains("private-test-signature")
    );
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_database_does_not_block_reactor_and_cancelled_change_is_not_committed() {
    use std::{sync::Arc, time::Duration};

    let root = tempfile::tempdir().unwrap();
    let center = Arc::new(ToolCenter::open(root.path()).unwrap());
    let connection = rusqlite::Connection::open(root.path().join("tools.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let active = Arc::clone(&center);
    let pending = tokio::spawn(async move {
        active
            .execute(
                "admin",
                request(0, ToolCenterCommand::SetPolicy { enabled: true }),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!pending.is_finished());
    pending.abort();
    let _ = pending.await;
    connection.execute_batch("ROLLBACK").unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!center.snapshot().unwrap().enabled);
    assert_eq!(center.snapshot().unwrap().revision, 0);
}

#[tokio::test]
async fn committed_revisions_publish_exact_events_and_cache_without_coalescing_disable() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let mut events = center.subscribe();
    assert_eq!(center.cached_catalog().revision, 0);
    for enabled in [true, false, true] {
        let response = run(&center, ToolCenterCommand::SetPolicy { enabled }).await;
        assert!(response.error.is_none());
        assert_eq!(center.cached_catalog().revision, response.revision);
        assert_eq!(center.cached_catalog().enabled, enabled);
    }
    for (revision, enabled) in [(1, true), (2, false), (3, true)] {
        let event = events.try_recv().unwrap();
        assert_eq!(event.revision, revision);
        assert_eq!(event.enabled, enabled);
    }
    assert!(events.try_recv().is_err());
    let operation = request(3, ToolCenterCommand::SetPolicy { enabled: false });
    let response = center.execute("admin", operation.clone()).await;
    assert!(response.error.is_none());
    assert_eq!(events.try_recv().unwrap().revision, 4);
    assert_eq!(center.execute("admin", operation).await, response);
    assert!(events.try_recv().is_err(), "幂等重放不产生重复事件");
    let stale = center
        .execute(
            "admin",
            request(2, ToolCenterCommand::SetPolicy { enabled: true }),
        )
        .await;
    assert_eq!(stale.error.unwrap().code, "revision_conflict");
    assert!(events.try_recv().is_err());
    let reopened = ToolCenter::open(root.path()).unwrap();
    assert_eq!(reopened.cached_catalog(), center.cached_catalog());
}

#[tokio::test]
async fn authorization_revoked_while_waiting_for_database_prevents_commit() {
    use std::{
        sync::{Arc, RwLock},
        time::Duration,
    };

    let root = tempfile::tempdir().unwrap();
    let center = Arc::new(ToolCenter::open(root.path()).unwrap());
    let mut events = center.subscribe();
    let connection = rusqlite::Connection::open(root.path().join("tools.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let token = Arc::new(RwLock::new(true));
    let authorization = Arc::clone(&token);
    let active = Arc::clone(&center);
    let operation = tokio::spawn(async move {
        active
            .execute_authorized(
                "mcp:test",
                request(0, ToolCenterCommand::SetPolicy { enabled: true }),
                authorization,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    *token.write().unwrap() = false;
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        operation.await.unwrap().error.unwrap().code,
        "tool_management_denied"
    );
    assert!(!center.snapshot().unwrap().enabled);
    assert_eq!(center.cached_catalog().revision, 0);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn edit_execution_definition_invalidates_previous_platform_evidence() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let receipt = upload(&center, &archive("tool.exe", b"test")).await;
    let tool = managed(&receipt);
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool })
            .await
            .error
            .is_none()
    );
    let record = PlatformTest {
        os: "windows".into(),
        arch: "x86_64".into(),
        environment: "Windows actual desktop".into(),
        desktop: true,
        windows_build: Some(26100),
        status: "passed".into(),
        result: "tested actual operation".into(),
        tested_at: chrono::Utc::now().to_rfc3339(),
        recorded_by: String::new(),
    };
    assert!(
        run(
            &center,
            ToolCenterCommand::RecordTest {
                tool_id: "example".into(),
                version: "1.0.0".into(),
                sha256: receipt.sha256,
                record
            }
        )
        .await
        .error
        .is_none()
    );
    let mut tool = center.snapshot().unwrap().tools.remove(0);
    assert_eq!(
        tool.versions[0].packages[0].tool.platforms[0].status,
        "passed"
    );
    tool.versions[0].packages[0]
        .tool
        .startup
        .args
        .push("--new-behavior".into());
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool })
            .await
            .error
            .is_none()
    );
    let snapshot = center.snapshot().unwrap();
    assert_eq!(
        snapshot.tools[0].versions[0].packages[0].tool.platforms[0].status,
        "pending"
    );
    assert_eq!(
        snapshot.tools[0].versions[0].packages[0].tests,
        Vec::<model::PlatformTest>::new()
    );
}

#[tokio::test]
async fn browser_adapter_is_required_only_for_new_or_changed_browser_startup() {
    use remoteops_resources::{BrowserAdapter, ExecutionInterface};
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let receipt = upload(&center, &archive("tool.exe", b"test")).await;
    let mut tool = managed(&receipt);
    let startup = &mut tool.versions[0].packages[0].tool.startup;
    startup.interface = ExecutionInterface::StdioMcp;
    startup.browser_executable = "browser.exe".into();
    startup.browser_adapter = None;
    assert_eq!(
        run(&center, ToolCenterCommand::PutTool { tool: tool.clone() })
            .await
            .error
            .unwrap()
            .code,
        "browser_adapter_required"
    );
    tool.versions[0].packages[0].tool.startup.browser_adapter = Some(BrowserAdapter::Playwright);
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool: tool.clone() })
            .await
            .error
            .is_none()
    );
    // 模拟升级前数据库，不经新登记入口写入旧资料。
    tool.versions[0].packages[0].tool.startup.browser_adapter = None;
    drop(center);
    let db = rusqlite::Connection::open(root.path().join("tools.sqlite3")).unwrap();
    db.execute(
        "UPDATE tools SET document=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_string(&tool).unwrap(), tool.id],
    )
    .unwrap();
    drop(db);
    let center = ToolCenter::open(root.path()).unwrap();
    assert!(center.catalog().is_ok());
    tool.instructions = "仅修改说明".into();
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool: tool.clone() })
            .await
            .error
            .is_none()
    );
    tool.versions[0].packages[0]
        .tool
        .startup
        .args
        .push("changed".into());
    assert_eq!(
        run(&center, ToolCenterCommand::PutTool { tool: tool.clone() })
            .await
            .error
            .unwrap()
            .code,
        "browser_adapter_required"
    );
    tool.versions[0].packages[0].tool.startup.browser_adapter = Some(BrowserAdapter::AgentBrowser);
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool: tool.clone() })
            .await
            .error
            .is_none()
    );
    tool.versions[0].packages[0].tool.startup.browser_adapter = None;
    tool.versions[0].packages[0]
        .tool
        .startup
        .browser_executable
        .clear();
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool })
            .await
            .error
            .is_none()
    );
}

async fn register_contract_example(center: &ToolCenter, example: &str, receipt: &PackageReceipt) {
    let mut input: Value = serde_json::from_str(example).unwrap();
    input["expected_revision"] = json!(center.snapshot().unwrap().revision);
    let package = &mut input["command"]["tool"]["versions"][0]["packages"][0];
    package["source"] = json!(receipt.source);
    package["tool"]["package"]["size"] = json!(receipt.size);
    package["tool"]["package"]["sha256"] = json!(receipt.sha256);
    package["tool"]["package"]["url"] = json!(match &receipt.source {
        PackageSource::Hosted => format!("relay://packages/{}.zip", receipt.sha256),
        PackageSource::External { url } => url.clone(),
    });
    let request: ToolCenterRequest = serde_json::from_value(input).unwrap();
    let saved = center.execute("admin:contract-fixture", request).await;
    assert!(saved.error.is_none(), "{:?}", saved.error);
    let snapshot = center.snapshot().unwrap();
    assert!(!snapshot.enabled);
    assert!(!snapshot.tools[0].enabled);
    let package = &snapshot.tools[0].versions[0].packages[0];
    assert!(package.file_verified);
    assert_eq!(package.tests, Vec::<model::PlatformTest>::new());
    assert_eq!(package.tool.platforms[0].status, "pending");
    assert_eq!(
        center.catalog().unwrap().tools,
        Vec::<remoteops_resources::ResourceTool>::new()
    );

    let mut input: Value = serde_json::from_str(include_str!(
        "../../../deploy/skills/remoteops/references/examples/record-test.json"
    ))
    .unwrap();
    input["expected_revision"] = json!(saved.revision);
    input["command"]["sha256"] = json!(receipt.sha256);
    let request: ToolCenterRequest = serde_json::from_value(input).unwrap();
    let recorded = center.execute("admin:contract-fixture", request).await;
    assert!(recorded.error.is_none(), "{:?}", recorded.error);
    assert_eq!(recorded.revision, saved.revision + 1);
    let snapshot = center.snapshot().unwrap();
    assert!(!snapshot.enabled);
    assert!(!snapshot.tools[0].enabled);
    let package = &snapshot.tools[0].versions[0].packages[0];
    assert_eq!(package.tests.len(), 1);
    assert_eq!(package.tests[0].recorded_by, "admin:contract-fixture");
    assert_eq!(package.tests[0].status, "pending");
    assert_eq!(package.tool.platforms[0].status, "pending");
    assert_eq!(
        center.catalog().unwrap().tools,
        Vec::<remoteops_resources::ResourceTool>::new()
    );
}

#[tokio::test]
async fn hosted_contract_example_registers_and_records_pending_test_in_isolated_store() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let receipt = upload(&center, &archive("bin/tool.exe", b"not executed")).await;
    register_contract_example(
        &center,
        include_str!("../../../deploy/skills/remoteops/references/examples/put-tool-hosted.json"),
        &receipt,
    )
    .await;
}

#[tokio::test]
async fn exact_build_history_is_not_collapsed_in_authenticated_catalog() {
    let root = tempfile::tempdir().unwrap();
    let center = ToolCenter::open(root.path()).unwrap();
    let receipt = upload(&center, &archive("payload.exe", b"platform-fixture")).await;
    let tool = managed(&receipt);
    let id = tool.id.clone();
    let version = tool.versions[0].version.clone();
    let sha = tool.versions[0].packages[0].tool.package.sha256.clone();
    assert!(
        run(&center, ToolCenterCommand::PutTool { tool })
            .await
            .error
            .is_none()
    );
    for (build, status) in [(26100, "passed"), (26200, "failed"), (26300, "passed")] {
        let record = PlatformTest {
            os: "windows".into(),
            arch: "x86_64".into(),
            environment: format!("Windows build {build}"),
            desktop: true,
            windows_build: Some(build),
            status: status.into(),
            result: "synthetic behavior test".into(),
            tested_at: chrono::Utc::now().to_rfc3339(),
            recorded_by: String::new(),
        };
        assert!(
            run(
                &center,
                ToolCenterCommand::RecordTest {
                    tool_id: id.clone(),
                    version: version.clone(),
                    sha256: sha.clone(),
                    record
                }
            )
            .await
            .error
            .is_none()
        );
    }
    assert!(
        run(&center, ToolCenterCommand::SetPolicy { enabled: true })
            .await
            .error
            .is_none()
    );
    assert!(
        run(
            &center,
            ToolCenterCommand::SetToolEnabled {
                tool_id: id,
                enabled: true
            }
        )
        .await
        .error
        .is_none()
    );
    let tool = &center.catalog().unwrap().tools[0];
    assert_eq!(tool.platform_tests.len(), 3);
    assert_eq!(
        tool.assess_platform("windows", "x86_64", Some(26100))
            .status,
        remoteops_resources::PlatformState::Verified
    );
    assert_eq!(
        tool.assess_platform("windows", "x86_64", Some(26200))
            .status,
        remoteops_resources::PlatformState::Failed
    );
    assert_eq!(
        tool.assess_platform("windows", "x86_64", Some(26300))
            .status,
        remoteops_resources::PlatformState::Verified
    );
}
