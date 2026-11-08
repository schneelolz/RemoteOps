use std::{
    io::Write as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use rustls::pki_types::PrivatePkcs8KeyDer;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;

use crate::{ResourceConfig, ResourceManager};

struct HttpsFixture {
    origin: String,
    certificate: Vec<u8>,
    requests: Arc<AtomicUsize>,
    conditional: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpsFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpsFixture {
    async fn new(manifest: Vec<u8>, package: Vec<u8>, redirect: Option<String>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = certified.cert.der().to_vec();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certified.cert.der().clone()],
                PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into(),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let conditional = Arc::new(AtomicUsize::new(0));
        let request_counter = Arc::clone(&requests);
        let condition_counter = Arc::clone(&conditional);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                let manifest = manifest.clone();
                let package = package.clone();
                let redirect = redirect.clone();
                let requests = Arc::clone(&request_counter);
                let conditional = Arc::clone(&condition_counter);
                tokio::spawn(async move {
                    let Ok(mut socket) = acceptor.accept(socket).await else {
                        return;
                    };
                    let mut request = vec![0_u8; 16384];
                    let Ok(read) = socket.read(&mut request).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&request[..read]);
                    requests.fetch_add(1, Ordering::Relaxed);
                    let is_index = request
                        .split_whitespace()
                        .nth(1)
                        .is_some_and(|path| path.ends_with("index.json"));
                    if let Some(redirect) = redirect {
                        let header = format!(
                            "HTTP/1.1 302 Found\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = socket.write_all(header.as_bytes()).await;
                    } else if is_index
                        && request
                            .to_ascii_lowercase()
                            .contains("if-none-match: \"test\"")
                    {
                        conditional.fetch_add(1, Ordering::Relaxed);
                        let _ = socket
                            .write_all(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n")
                            .await;
                    } else {
                        let bytes = if is_index { manifest } else { package };
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"test\"\r\nLast-Modified: Wed, 05 Nov 2025 00:00:00 GMT\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                        if socket.write_all(header.as_bytes()).await.is_ok() {
                            let _ = socket.write_all(&bytes).await;
                        }
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            origin,
            certificate,
            requests,
            conditional,
            task,
        }
    }

    fn manager(&self, root: &std::path::Path, extra: Option<&Self>) -> ResourceManager {
        let mut config = ResourceConfig {
            index_url: Some(format!("{}/remoteops/v1/index.json", self.origin)),
            allowed_origins: vec![self.origin.clone()],
        };
        let mut client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(reqwest::Certificate::from_der(&self.certificate).unwrap());
        if let Some(other) = extra {
            config.allowed_origins.push(other.origin.clone());
            client = client
                .add_root_certificate(reqwest::Certificate::from_der(&other.certificate).unwrap());
        }
        let mut manager = ResourceManager::new(config, root.into()).unwrap();
        manager.client = client.build().unwrap();
        manager
    }
}

#[tokio::test]
async fn two_https_origins_etag_cache_and_redirect_trust_are_enforced() {
    let first = HttpsFixture::new(
        br#"{"schema_version":1,"tools":[]}"#.to_vec(),
        Vec::new(),
        None,
    )
    .await;
    let second = HttpsFixture::new(
        br#"{"schema_version":1,"tools":[],"extra":true}"#.to_vec(),
        Vec::new(),
        None,
    )
    .await;
    let root = tempfile::tempdir().unwrap();
    let manager = first.manager(root.path(), None);
    manager.list_tools().await.unwrap();
    manager.list_tools().await.unwrap();
    assert_eq!(first.conditional.load(Ordering::Relaxed), 1);
    second
        .manager(root.path(), None)
        .list_tools()
        .await
        .unwrap();
    assert_eq!(second.conditional.load(Ordering::Relaxed), 0);
    let redirect = HttpsFixture::new(
        Vec::new(),
        Vec::new(),
        Some(format!("{}/remoteops/v1/index.json", second.origin)),
    )
    .await;
    let error = redirect
        .manager(root.path(), None)
        .list_tools()
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_denied");
    assert_eq!(second.requests.load(Ordering::Relaxed), 1);
    redirect
        .manager(root.path(), Some(&second))
        .list_tools()
        .await
        .unwrap();
    assert_eq!(second.requests.load(Ordering::Relaxed), 2);
    let moved = HttpsFixture::new(
        Vec::new(),
        Vec::new(),
        Some(format!("{}/moved/index.json", second.origin)),
    )
    .await;
    let manager = moved.manager(root.path(), Some(&second));
    let (base, _) = crate::cache::fetch_index(&manager.client, &manager.config, root.path())
        .await
        .unwrap();
    assert_eq!(base.path(), "/moved/index.json");
    assert_eq!(
        base.join("packages/tool.zip").unwrap().path(),
        "/moved/packages/tool.zip"
    );
}

#[tokio::test]
async fn bounded_download_digest_and_cache_damage_fail_before_execution() {
    use sha2::{Digest as _, Sha256};
    let sample = include_bytes!("../../../examples/resource-site/remoteops/v1/index.json");
    let mut index = crate::validate_manifest(sample).unwrap();
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for name in ["node/node.exe", "browser/chrome-win64/chrome.exe"] {
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(b"fixture bytes, never executable")
            .unwrap();
    }
    let archive = archive.finish().unwrap().into_inner();
    index.tools.truncate(1);
    let tool = &mut index.tools[0];
    tool.package.size = archive.len() as u64;
    tool.package.sha256 = format!("{:x}", Sha256::digest(&archive));
    let site = HttpsFixture::new(serde_json::to_vec(&index).unwrap(), archive, None).await;
    let directory = tempfile::tempdir().unwrap();
    let manager = site.manager(directory.path(), None);
    let url = manager.config.index().unwrap();
    let package = crate::cache::package(
        &manager.client,
        &manager.config,
        &url,
        &index.tools[0],
        directory.path(),
    )
    .await
    .unwrap();
    crate::cache::package(
        &manager.client,
        &manager.config,
        &url,
        &index.tools[0],
        directory.path(),
    )
    .await
    .unwrap();
    assert_eq!(site.requests.load(Ordering::Relaxed), 1);
    std::fs::write(package.join("node/node.exe"), b"changed").unwrap();
    assert_eq!(
        crate::cache::package(
            &manager.client,
            &manager.config,
            &url,
            &index.tools[0],
            directory.path()
        )
        .await
        .unwrap_err()
        .code,
        "cache_corrupt"
    );
    let mut bad = index.tools[0].clone();
    bad.package.sha256 = "0".repeat(64);
    assert_eq!(
        crate::cache::package(
            &manager.client,
            &manager.config,
            &url,
            &bad,
            directory.path()
        )
        .await
        .unwrap_err()
        .code,
        "hash_mismatch"
    );
    bad.package.size -= 1;
    assert_eq!(
        crate::cache::package(
            &manager.client,
            &manager.config,
            &url,
            &bad,
            directory.path()
        )
        .await
        .unwrap_err()
        .code,
        "size_mismatch"
    );
    assert_eq!(index.tools[0].package.sha256.len(), 64);
}

#[cfg(windows)]
fn instance(
    tool: crate::ResourceTool,
    package: std::path::PathBuf,
    parent: &std::path::Path,
    cleanup: Arc<crate::cleanup::CleanupWorkers>,
) -> super::Instance {
    let run = tempfile::Builder::new()
        .prefix("browser-session-")
        .tempdir_in(parent)
        .unwrap();
    std::fs::create_dir(run.path().join("profile")).unwrap();
    super::Instance {
        tool,
        package,
        process: None,
        input: None,
        output: None,
        tools: Vec::new(),
        sequence: 0,
        password_binding: None,
        password_redactions: Vec::new(),
        run: Some(run),
        startup_trace: Arc::default(),
        cleanup,
        job: None,
    }
}

#[cfg(windows)]
async fn live_candidate_processes(package: &std::path::Path) -> usize {
    let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let script = r#"function NormalizePath([string]$path) { if($path.StartsWith('\\?\')) { return $path.Substring(4) }; return $path }; $prefix=$env:REMOTEOPS_TEST_PACKAGE; $count=0; $rows=@(); Get-CimInstance Win32_Process -Filter "Name='chrome.exe' OR Name='node.exe' OR Name='agent-browser-win32-x64.exe'" | Where-Object { $_.ExecutablePath -and (NormalizePath $_.ExecutablePath).StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { $p=Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue; if($p -and $p.Path -and (NormalizePath $p.Path).StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase)) { $exited=$p.HasExited; if(-not $exited) { $count++ }; $rows+=@{pid=$_.ProcessId;parent=$_.ParentProcessId;created=$_.CreationDate.ToUniversalTime().ToString('o');path=$p.Path;has_exited=$exited} } }; @{count=$count;processes=$rows}|ConvertTo-Json -Compress -Depth 4"#;
    let mut command =
        tokio::process::Command::new(root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("REMOTEOPS_TEST_PACKAGE", package)
        .creation_flags(0x0800_0000)
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    let observed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    if observed["processes"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        eprintln!("固定候选进程核实: {observed}");
    }
    observed["count"].as_u64().unwrap().try_into().unwrap()
}

#[tokio::test]
async fn stopping_one_session_cancels_only_its_pending_start() {
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let manager =
        Arc::new(ResourceManager::new(ResourceConfig::default(), directory.path().into()).unwrap());
    let preparation = manager.preparation.lock().await;
    let first_manager = Arc::clone(&manager);
    let first = tokio::spawn(async move {
        first_manager
            .start("session-a", "test", "1", "digest")
            .await
    });
    let second_manager = Arc::clone(&manager);
    let second = tokio::spawn(async move {
        second_manager
            .start("session-b", "test", "1", "digest")
            .await
    });
    for _ in 0..100 {
        if manager.starting.lock().unwrap().len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(manager.starting.lock().unwrap().len(), 2);
    manager.stop_session("session-a").await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "startup_cancelled");
    assert!(!second.is_finished());
    assert_eq!(manager.starting.lock().unwrap().len(), 1);
    drop(preparation);
    assert_eq!(second.await.unwrap().unwrap_err().code, "disabled");
    assert!(manager.starting.lock().unwrap().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn cancelled_calls_release_capacity_and_never_become_retry_safe() {
    use serde_json::json;
    use std::{path::PathBuf, process::Stdio, time::Duration};
    let directory = tempfile::tempdir().unwrap();
    let manager =
        ResourceManager::new(ResourceConfig::default(), directory.path().join("cache")).unwrap();
    let index = crate::validate_manifest(include_bytes!(
        "../../../examples/resource-site/remoteops/v1/index.json"
    ))
    .unwrap();
    for number in 0..10 {
        let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let mut command = tokio::process::Command::new(
            root.join("System32/WindowsPowerShell/v1.0/powershell.exe"),
        );
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::ReadLine() | Out-Null; Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = super::spawn(command).unwrap();
        let mut candidate = instance(
            index.tools[0].clone(),
            root,
            directory.path(),
            Arc::clone(&manager.cleanup),
        );
        candidate.input = process.child.stdin().take();
        candidate.output = process.child.stdout().take().map(tokio::io::BufReader::new);
        candidate.process = Some(process);
        candidate.tools.push(super::CallToolSchema {
            name: "fixture".into(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
        });
        let id = format!("cancelled-{number}");
        let (cancel, _) = tokio::sync::watch::channel(false);
        manager.instances().insert(
            id.clone(),
            Arc::new(super::Handle {
                identity: ("fixture".into(), "1".into(), "hash".into()),
                session: "session-a".into(),
                cancel,
                instance: tokio::sync::Mutex::new(Some(candidate)),
            }),
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                manager.call("session-a", &id, "fixture", json!({}))
            )
            .await
            .is_err()
        );
        assert!(manager.instances().is_empty());
        let repeated = manager
            .call("session-a", &id, "fixture", json!({}))
            .await
            .unwrap_err();
        assert_eq!(repeated.code, "instance_missing");
        assert!(!repeated.retry_safe);
        assert_eq!(
            manager
                .start("session-a", "test", "1", "digest")
                .await
                .unwrap_err()
                .code,
            "disabled"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
#[allow(clippy::too_many_lines)]
#[ignore = "需要 scripts/build-resource-packages.py 生成的真实固定候选包"]
async fn real_candidates_headless_navigation_state_isolation_and_unknown_results() {
    use serde_json::json;
    use std::{path::PathBuf, time::Duration};

    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page = format!("http://127.0.0.1:{}/", fixture.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = fixture.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = [0_u8; 8192];
                let _ = socket.read(&mut request).await;
                let html = r#"<!doctype html><title>RemoteOps Resource Fixture</title><label>Name<input id="name" aria-label="Name"></label><label>Password<input type="password" id="password" aria-label="Password"></label><button onclick="document.title='RemoteOps submitted'">Apply</button>"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    let site = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/resource-candidates/site/remoteops/v1");
    let mut index =
        crate::validate_manifest(&std::fs::read(site.join("index.json")).unwrap()).unwrap();
    let mut agent_mcp = index
        .tools
        .iter()
        .find(|tool| tool.id == "agent-browser")
        .unwrap()
        .clone();
    agent_mcp.id = "agent-browser-mcp".into();
    agent_mcp.startup.interface = crate::ExecutionInterface::StdioMcp;
    agent_mcp.startup.browser_adapter = Some(crate::BrowserAdapter::AgentBrowser);
    agent_mcp.startup.args = vec!["mcp".into(), "--tools".into(), "core".into()];
    agent_mcp.health_check.method = "tools/list".into();
    index.tools.push(agent_mcp);
    let directory = tempfile::tempdir().unwrap();
    let manager =
        ResourceManager::new(ResourceConfig::default(), directory.path().join("cache")).unwrap();
    for mut tool in index.tools {
        let name = tool.id.clone();
        let package = directory.path().join(&name);
        let process_prefix = package.clone();
        std::fs::create_dir(&package).unwrap();
        let archive = site.join(&tool.package.url);
        let digest = tool.package.sha256.clone();
        let unpack = package.clone();
        tokio::task::spawn_blocking(move || crate::cache::extract(&archive, &unpack, &digest))
            .await
            .unwrap()
            .unwrap();
        tool.requires_desktop = false;
        tool.timeout_secs = 30;
        if name == "playwright-mcp" {
            tool.startup.args.push("--headless".into());
        }
        let mut instance = instance(
            tool,
            package,
            directory.path(),
            Arc::clone(&manager.cleanup),
        );
        tokio::time::timeout(Duration::from_mins(1), instance.initialize())
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("{name} 初始化失败: {error}"));
        instance.trace().finish();
        let run_path = instance.run_path().to_owned();
        let (cancel, _) = tokio::sync::watch::channel(false);
        let instance_id = name.clone();
        manager.instances().insert(
            instance_id.clone(),
            Arc::new(super::Handle {
                identity: ("fixture".into(), "1".into(), "hash".into()),
                session: "session-a".into(),
                cancel,
                instance: tokio::sync::Mutex::new(Some(instance)),
            }),
        );
        assert_eq!(
            manager
                .call("session-b", &instance_id, "browser_snapshot", json!({}))
                .await
                .unwrap_err()
                .code,
            "session_mismatch"
        );
        assert_eq!(
            manager
                .stop("session-b", &instance_id)
                .await
                .unwrap_err()
                .code,
            "session_mismatch"
        );
        let cli = name == "agent-browser";
        let agent_mcp = name == "agent-browser-mcp";
        let navigate = if cli {
            "browser_open"
        } else if agent_mcp {
            "agent_browser_open"
        } else {
            "browser_navigate"
        };
        let opened = manager
            .call("session-a", &instance_id, navigate, json!({"url":page}))
            .await
            .unwrap_or_else(|error| panic!("{name} 打开失败: {error}"));
        assert!(
            !opened
                .get("isError")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            "{name}: {opened}"
        );
        let snapshot = manager
            .call(
                "session-a",
                &instance_id,
                if agent_mcp {
                    "agent_browser_snapshot"
                } else {
                    "browser_snapshot"
                },
                json!({}),
            )
            .await
            .unwrap();
        assert!(snapshot.to_string().contains("Name"), "{name}: {snapshot}");
        verify_real_password(&manager, &instance_id, &page, cli, agent_mcp).await;
        let body = "document.querySelector('input').value='RemoteOps'; document.querySelector('button').click(); return {title:document.title,value:document.querySelector('input').value};";
        if agent_mcp {
            let title = manager
                .call(
                    "session-a",
                    &instance_id,
                    "agent_browser_get_title",
                    json!({}),
                )
                .await
                .unwrap();
            assert!(title.to_string().contains("RemoteOps Resource Fixture"));
            let url = manager
                .call(
                    "session-a",
                    &instance_id,
                    "agent_browser_get_url",
                    json!({}),
                )
                .await
                .unwrap();
            assert!(url.to_string().contains(&page));
            let screenshot = manager
                .call(
                    "session-a",
                    &instance_id,
                    "agent_browser_screenshot",
                    json!({"path":run_path.join("test-shot.png")}),
                )
                .await
                .unwrap();
            assert_ne!(
                screenshot
                    .get("isError")
                    .and_then(serde_json::Value::as_bool),
                Some(true)
            );
            assert!(run_path.join("test-shot.png").exists());
        }
        let (evaluate, arguments) = if cli || agent_mcp {
            (
                if agent_mcp {
                    "agent_browser_eval"
                } else {
                    "browser_eval"
                },
                json!({"script":format!("(() => {{ {body} }})()") }),
            )
        } else {
            (
                "browser_evaluate",
                json!({"function":format!("() => {{ {body} }}") }),
            )
        };
        let submitted = manager
            .call("session-a", &instance_id, evaluate, arguments)
            .await
            .unwrap();
        assert!(
            submitted.to_string().contains("RemoteOps submitted"),
            "{name}: {submitted}"
        );
        let handle = manager.instances().get(&instance_id).cloned().unwrap();
        handle
            .instance
            .lock()
            .await
            .as_mut()
            .unwrap()
            .tool
            .timeout_secs = 1;
        let arguments = if cli || agent_mcp {
            json!({"script":"new Promise(() => {})"})
        } else {
            json!({"function":"() => new Promise(() => {})"})
        };
        if cli {
            let cancelled = tokio::time::timeout(
                Duration::from_millis(100),
                manager.call("session-a", &instance_id, evaluate, arguments),
            )
            .await;
            assert!(cancelled.is_err(), "测试应取消已提交的调用");
        } else {
            let error = manager
                .call("session-a", &instance_id, evaluate, arguments)
                .await
                .unwrap_err();
            assert_eq!(error.code, "result_unknown");
            assert!(!error.retry_safe);
        }
        let repeated = manager
            .call("session-a", &instance_id, evaluate, json!({}))
            .await
            .unwrap_err();
        assert_eq!(repeated.code, "instance_missing");
        assert!(!repeated.retry_safe);
        manager.stop("session-a", &instance_id).await.unwrap();
        for _ in 0..1200 {
            if !run_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!run_path.exists(), "{name} 运行目录/profile 未回收");
        // Windows Job 终止是异步的；核实真实退出，不能仅凭 profile 已删除判断。
        let process_wait = tokio::time::Instant::now();
        let mut live = live_candidate_processes(&process_prefix).await;
        while live != 0 && process_wait.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            live = live_candidate_processes(&process_prefix).await;
        }
        assert_eq!(live, 0, "{name} 仍有存活的固定候选进程");
        eprintln!("{name}: 真实进程退出核实耗时 {:?}", process_wait.elapsed());
        eprintln!(
            "{name}: 固定候选 headless 导航、快照、表单交互、跨会话拒绝、未知结果不重放、profile 回收通过"
        );
    }
    manager.stop_all().await.unwrap();
    server.abort();
}

fn native_fixture() -> &'static std::path::Path {
    static FIXTURE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap().keep();
        let binary = directory.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        let status = std::process::Command::new("rustc")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/native-tool.rs"),
            )
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success());
        binary
    })
}

fn central_tool(interface: &str) -> (crate::ResourceTool, Vec<u8>) {
    use serde_json::json;
    use sha2::{Digest as _, Sha256};
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file(
            "tool.exe",
            zip::write::SimpleFileOptions::default().unix_permissions(0o755),
        )
        .unwrap();
    archive
        .write_all(&std::fs::read(native_fixture()).unwrap())
        .unwrap();
    let bytes = archive.finish().unwrap().into_inner();
    let mut tool = crate::validate_manifest(include_bytes!(
        "../../../examples/resource-site/remoteops/v1/index.json"
    ))
    .unwrap()
    .tools
    .remove(0);
    tool.id = "fixture".into();
    tool.requires_desktop = false;
    tool.platforms = vec![crate::Platform {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        os_versions: "test host".into(),
        status: "passed".into(),
        min_windows_build: Some(0),
        max_windows_build: Some(u32::MAX),
    }];
    tool.package.size = bytes.len() as u64;
    tool.package.sha256 = format!("{:x}", Sha256::digest(&bytes));
    tool.package.url = format!("relay://packages/{}.zip", tool.package.sha256);
    tool.startup.executable = "tool.exe".into();
    tool.startup.browser_executable.clear();
    tool.startup.browser_adapter = None;
    tool.startup.args.clear();
    tool.startup.interface = serde_json::from_value(json!(interface)).unwrap();
    if interface == "cli" {
        tool.health_check.method = "schema".into();
        tool.startup.operations = serde_json::from_value(json!([{"name":"echo","description":"echo","input_schema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"args":[{"kind":"literal","value":"cli"},{"kind":"parameter","name":"text"}]}])).unwrap();
    }
    (tool, bytes)
}

fn central_manager(
    root: &std::path::Path,
    bytes: Vec<u8>,
    downloads: Arc<AtomicUsize>,
) -> ResourceManager {
    let manager = ResourceManager::new_centralized(root.into()).unwrap();
    manager.set_package_fetcher(Arc::new(move |_, _progress| {
        let bytes = bytes.clone();
        let downloads = downloads.clone();
        Box::pin(async move {
            downloads.fetch_add(1, Ordering::Relaxed);
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(&bytes).unwrap();
            Ok(file.into_temp_path().keep().unwrap())
        })
    }));
    manager
}

#[tokio::test]
async fn explicit_mcp_adapters_and_failure_diagnostics_are_observable() {
    use crate::BrowserAdapter;
    use serde_json::json;
    for (mode, reason) in [
        ("exit", "eof"),
        ("invalid-json", "invalid_json"),
        ("upstream-error", "upstream_error"),
        ("timeout", "timeout"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut tool, bytes) = central_tool("stdio_mcp");
        tool.startup.browser_adapter = Some(BrowserAdapter::None);
        tool.startup.args = vec![mode.into()];
        tool.health_check.timeout_secs = if mode == "timeout" { 1 } else { 5 };
        let manager = central_manager(root.path(), bytes, Arc::default());
        manager
            .apply_catalog(1, true, vec![tool.clone()])
            .await
            .unwrap();
        let error = manager
            .start("session", &tool.id, &tool.version, &tool.package.sha256)
            .await
            .unwrap_err();
        let diagnostic = error.diagnostics.unwrap();
        assert_eq!(diagnostic.stage, "initialize", "{mode}");
        let context = diagnostic.launch_context.as_ref().unwrap();
        assert_eq!(context.browser_adapter.as_deref(), Some("none"));
        assert_eq!(context.adapter_source, "explicit");
        assert!(context.injected_args.is_empty());
        assert_eq!(diagnostic.reason, reason, "{mode}");
        assert_eq!(diagnostic.last_sent.unwrap()["method"], "initialize");
        if mode == "exit" {
            assert_eq!(diagnostic.exit_code, Some(23));
            assert!(diagnostic.stderr_tail.contains("fixture startup failed"));
            assert!(!diagnostic.stderr_tail.contains("private"));
        }
        assert!(!diagnostic.stderr_tail.contains("must-not-leak"));
        let record = root
            .path()
            .join("diagnostics")
            .join(diagnostic.record_id.unwrap());
        let saved = std::fs::read_to_string(record).unwrap();
        assert!(!saved.contains("must-not-leak"));
        assert!(saved.len() <= 65536);
        manager.stop_all().await.unwrap();
        assert_eq!(
            std::fs::read_dir(root.path().join("runs")).unwrap().count(),
            0
        );
    }
    for (mode, adapter) in [
        ("check-agent", BrowserAdapter::AgentBrowser),
        ("stderr-flood", BrowserAdapter::None),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut tool, bytes) = central_tool("stdio_mcp");
        tool.startup.browser_adapter = Some(adapter);
        tool.startup.args = vec![mode.into()];
        if adapter == BrowserAdapter::AgentBrowser {
            tool.startup.browser_executable = "tool.exe".into();
        }
        let manager = central_manager(root.path(), bytes, Arc::default());
        manager
            .apply_catalog(1, true, vec![tool.clone()])
            .await
            .unwrap();
        let started = manager
            .start("session", &tool.id, &tool.version, &tool.package.sha256)
            .await
            .unwrap();
        assert_eq!(started.tools[0].name, "fixture_echo");
        manager
            .apply_catalog_with_guide(2, true, vec![tool.clone()], "# Updated guide".into())
            .await
            .unwrap();
        let (guide, tools) = manager.list_catalog().await.unwrap();
        assert_eq!(guide, "# Updated guide");
        assert_eq!(tools[0].id, tool.id);
        manager
            .call("session", &started.instance_id, "fixture_echo", json!({}))
            .await
            .unwrap();
        manager.stop_all().await.unwrap();
        assert!(!root.path().join("diagnostics").exists());
    }
}

#[tokio::test]
async fn launch_context_identifies_explicit_and_legacy_adapters_without_private_values() {
    use crate::BrowserAdapter;
    for adapter in [
        None,
        Some(BrowserAdapter::Playwright),
        Some(BrowserAdapter::AgentBrowser),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut tool, bytes) = central_tool("stdio_mcp");
        tool.startup.browser_adapter = adapter;
        tool.startup.browser_executable = "tool.exe".into();
        tool.startup.args = vec!["exit".into()];
        let manager = central_manager(root.path(), bytes, Arc::default());
        manager
            .apply_catalog(1, true, vec![tool.clone()])
            .await
            .unwrap();
        let error = manager
            .start("session", &tool.id, &tool.version, &tool.package.sha256)
            .await
            .unwrap_err();
        let diagnostic = error.diagnostics.unwrap();
        let context = diagnostic.launch_context.unwrap();
        assert_eq!(
            context.adapter_source,
            if adapter.is_some() {
                "explicit"
            } else {
                "legacy_default"
            }
        );
        let agent = adapter == Some(BrowserAdapter::AgentBrowser);
        assert_eq!(
            context.browser_adapter.as_deref(),
            Some(if agent { "agent_browser" } else { "playwright" })
        );
        assert_eq!(
            context.injected_args.iter().any(|arg| arg == "--browser"),
            !agent
        );
        assert_eq!(
            context
                .injected_env_names
                .iter()
                .any(|name| name == "AGENT_BROWSER_EXECUTABLE_PATH"),
            agent
        );
        manager.stop_all().await.unwrap();
    }
}

#[test]
fn browser_adapter_contract_rejects_ambiguous_or_non_mcp_configuration() {
    for (interface, adapter, browser, valid) in [
        ("stdio_mcp", "none", "", true),
        ("stdio_mcp", "none", "tool.exe", false),
        ("stdio_mcp", "agent_browser", "tool.exe", true),
        ("stdio_mcp", "playwright", "", false),
        ("cli", "agent_browser", "tool.exe", false),
    ] {
        let (tool, _) = central_tool(interface);
        let mut value = serde_json::to_value(tool).unwrap();
        value["startup"]["browser_adapter"] = serde_json::json!(adapter);
        value["startup"]["browser_executable"] = serde_json::json!(browser);
        let bytes =
            serde_json::to_vec(&serde_json::json!({"schema_version":1,"tools":[value]})).unwrap();
        assert_eq!(
            crate::validate_manifest(&bytes).is_ok(),
            valid,
            "{interface}/{adapter}/{browser}"
        );
    }
}

#[tokio::test]
async fn centralized_native_cli_validates_arguments_caches_packages_and_reports_exit_streams() {
    use serde_json::json;
    let root = tempfile::tempdir().unwrap();
    let (tool, bytes) = central_tool("cli");
    let downloads = Arc::new(AtomicUsize::new(0));
    let manager = central_manager(root.path(), bytes, downloads.clone());
    assert_eq!(manager.list_tools().await.unwrap_err().code, "disabled");
    manager
        .apply_catalog(1, true, vec![tool.clone()])
        .await
        .unwrap();
    let instance = manager
        .start("a", &tool.id, &tool.version, &tool.package.sha256)
        .await
        .unwrap();
    assert_eq!(
        manager
            .call("a", &instance.instance_id, "echo", json!({"text":1}))
            .await
            .unwrap_err()
            .code,
        "invalid_call"
    );
    let output = manager
        .call(
            "a",
            &instance.instance_id,
            "echo",
            json!({"text":"one two; $(echo x)"}),
        )
        .await
        .unwrap();
    assert_eq!(output["exit_code"], 7);
    assert_eq!(output["stdout"], "one two; $(echo x)\n");
    assert_eq!(output["stderr"], "fixture-stderr\n");
    manager.stop("a", &instance.instance_id).await.unwrap();
    let instance = manager
        .start("a", &tool.id, &tool.version, &tool.package.sha256)
        .await
        .unwrap();
    assert_eq!(downloads.load(Ordering::Relaxed), 1);
    manager.apply_catalog(2, false, vec![tool]).await.unwrap();
    assert!(
        manager
            .call("a", &instance.instance_id, "echo", json!({"text":"x"}))
            .await
            .is_err()
    );
    assert_eq!(
        manager
            .apply_catalog(1, true, vec![])
            .await
            .unwrap_err()
            .code,
        "stale_catalog"
    );
    manager.stop_all().await.unwrap();
}

#[tokio::test]
async fn generic_mcp_initializes_without_browser_arguments_and_disable_cancels_submitted_call() {
    use serde_json::json;
    let root = tempfile::tempdir().unwrap();
    let (tool, bytes) = central_tool("stdio_mcp");
    let manager = Arc::new(central_manager(
        root.path(),
        bytes,
        Arc::new(AtomicUsize::new(0)),
    ));
    manager
        .apply_catalog(1, true, vec![tool.clone()])
        .await
        .unwrap();
    let instance = manager
        .start("a", &tool.id, &tool.version, &tool.package.sha256)
        .await
        .unwrap();
    assert_eq!(instance.tools[0].name, "fixture_echo");
    let result = manager
        .call("a", &instance.instance_id, "fixture_echo", json!({}))
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "generic MCP result");
    let id = instance.instance_id.clone();
    let worker = manager.clone();
    let call = tokio::spawn(async move {
        worker
            .call("a", &id, "fixture_echo", json!({"slow":true}))
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    manager.apply_catalog(2, false, vec![]).await.unwrap();
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(error.code, "result_unknown");
    assert!(!error.retry_safe);
    assert!(manager.instances().is_empty());
    manager.stop_all().await.unwrap();
}

#[tokio::test]
async fn synchronous_disable_cannot_be_coalesced_away_and_reconnect_resets_revision() {
    let root = tempfile::tempdir().unwrap();
    let (tool, bytes) = central_tool("cli");
    let manager = central_manager(root.path(), bytes, Arc::new(AtomicUsize::new(0)));
    manager
        .apply_catalog(50, true, vec![tool.clone()])
        .await
        .unwrap();
    let instance = manager
        .start("a", &tool.id, &tool.version, &tool.package.sha256)
        .await
        .unwrap();
    manager.publish_catalog(51, false, vec![]).unwrap();
    manager
        .publish_catalog(52, true, vec![tool.clone()])
        .unwrap();
    assert!(
        *manager
            .instances()
            .get(&instance.instance_id)
            .unwrap()
            .cancel
            .borrow()
    );
    manager
        .apply_catalog(52, true, vec![tool.clone()])
        .await
        .unwrap();
    assert!(manager.instances().is_empty());
    manager.reset_catalog();
    assert_eq!(manager.catalog_revision(), Some(0));
    assert_eq!(manager.list_tools().await.unwrap_err().code, "disabled");
    manager.apply_catalog(1, true, vec![tool]).await.unwrap();
    manager.stop_all().await.unwrap();
}

#[tokio::test]
async fn centralized_lists_hide_signed_url_parameters() {
    let root = tempfile::tempdir().unwrap();
    let (mut tool, _) = central_tool("cli");
    tool.package.url = "https://example.test/tools.zip?signature=fixture&expires=123".into();
    let manager = ResourceManager::new_centralized(root.path().into()).unwrap();
    manager.apply_catalog(1, true, vec![tool]).await.unwrap();
    assert_eq!(
        manager.list_tools().await.unwrap()[0].package.url,
        "https://example.test/tools.zip"
    );
}

#[tokio::test]
async fn synchronous_disable_cancels_package_download_before_startup() {
    let root = tempfile::tempdir().unwrap();
    let (tool, _) = central_tool("cli");
    let manager = Arc::new(ResourceManager::new_centralized(root.path().into()).unwrap());
    let downloading = Arc::new(tokio::sync::Notify::new());
    let notify = downloading.clone();
    manager.set_package_fetcher(Arc::new(move |_, _progress| {
        let notify = notify.clone();
        Box::pin(async move {
            notify.notify_one();
            std::future::pending().await
        })
    }));
    manager
        .apply_catalog(1, true, vec![tool.clone()])
        .await
        .unwrap();
    let worker = manager.clone();
    let start = tokio::spawn(async move {
        worker
            .start("a", &tool.id, &tool.version, &tool.package.sha256)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), downloading.notified())
        .await
        .unwrap();
    manager.publish_catalog(2, false, vec![]).unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), start)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "startup_cancelled");
    assert!(error.retry_safe);
    manager.stop_all().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn publishing_disable_drops_all_idle_processes_before_waiting_for_directory_cleanup() {
    use std::os::windows::fs::OpenOptionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let (tool, bytes) = central_tool("stdio_mcp");
    let manager = central_manager(root.path(), bytes, Arc::new(AtomicUsize::new(0)));
    manager
        .apply_catalog(1, true, vec![tool.clone()])
        .await
        .unwrap();
    let mut handles = Vec::new();
    let mut run_paths = Vec::new();
    for _ in 0..3 {
        let started = manager
            .start("a", &tool.id, &tool.version, &tool.package.sha256)
            .await
            .unwrap();
        let handle = manager
            .instances()
            .get(&started.instance_id)
            .cloned()
            .unwrap();
        run_paths.push(
            handle
                .instance
                .lock()
                .await
                .as_ref()
                .unwrap()
                .run_path()
                .to_path_buf(),
        );
        handles.push(handle);
    }
    // 第一实例的目录无法删除，不能阻止其余实例先被终止。
    let held = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .share_mode(0)
        .open(run_paths[0].join("held-open"))
        .unwrap();
    manager.publish_catalog(2, false, vec![]).unwrap();
    for handle in &handles {
        assert!(*handle.cancel.borrow());
        assert!(
            handle.instance.try_lock().unwrap().is_none(),
            "同步发布停用后所有空闲实例必须已经 Drop，不能等待先前实例清理"
        );
    }
    assert!(run_paths[0].exists(), "目录清理由后台等待文件释放");
    drop(held);
    manager.apply_catalog(2, false, vec![]).await.unwrap();
    for path in run_paths {
        assert!(!path.exists());
    }
    let package = root.path().join(&tool.package.sha256);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while live_candidate_processes(&package).await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "停用后仍有真实夹具进程存活"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    manager.stop_all().await.unwrap();
}

#[cfg(windows)]
async fn verify_real_password(
    manager: &ResourceManager,
    instance: &str,
    page: &str,
    cli: bool,
    agent_mcp: bool,
) {
    use serde_json::json;
    let origin = url::Url::parse(page)
        .unwrap()
        .origin()
        .ascii_serialization();
    let rejected = manager
        .prepare_browser_password(
            "session-a",
            instance,
            "http://wrong.invalid",
            "#password",
            "admin",
        )
        .await;
    assert!(rejected.is_err());
    let target = manager
        .prepare_browser_password("session-a", instance, &origin, "#password", "admin")
        .await
        .unwrap();
    let binding = target["binding_id"].as_str().unwrap();
    let secret = "RemoteOps-Test-秘密-\"'&<>-p13";
    let filled = manager
        .fill_browser_password("session-a", instance, binding, secret)
        .await
        .unwrap();
    assert_eq!(filled, json!({"status":"filled"}));
    assert!(
        manager
            .fill_browser_password("session-a", instance, binding, secret)
            .await
            .is_err()
    );
    let tool = if cli {
        "browser_eval"
    } else if agent_mcp {
        "agent_browser_eval"
    } else {
        "browser_evaluate"
    };
    let script = "document.querySelector('#password').value";
    let arguments = if cli || agent_mcp {
        json!({"script":script})
    } else {
        json!({"function":format!("() => {script}")})
    };
    let returned = manager
        .call("session-a", instance, tool, arguments)
        .await
        .unwrap()
        .to_string();
    assert!(
        !returned.contains("RemoteOps-Test"),
        "不得返回秘密：{returned}"
    );
    let target = manager
        .prepare_browser_password("session-a", instance, &origin, "#password", "admin")
        .await
        .unwrap();
    let change =
        "document.querySelector('#password').outerHTML='<input type=\"password\" id=\"password\">'";
    let arguments = if cli || agent_mcp {
        json!({"script":change})
    } else {
        json!({"function":format!("() => {{ {change}; }}")})
    };
    manager
        .call("session-a", instance, tool, arguments)
        .await
        .unwrap();
    assert!(
        manager
            .fill_browser_password(
                "session-a",
                instance,
                target["binding_id"].as_str().unwrap(),
                secret
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unverified_start_requires_explicit_validation_and_reports_real_preparation_stages() {
    let root = tempfile::tempdir().unwrap();
    let (mut tool, bytes) = central_tool("stdio_mcp");
    tool.startup.browser_adapter = Some(crate::BrowserAdapter::None);
    tool.platforms[0].status = "pending".into();
    let downloads = Arc::new(AtomicUsize::new(0));
    let manager = central_manager(root.path(), bytes, downloads.clone());
    manager
        .apply_catalog(1, true, vec![tool.clone()])
        .await
        .unwrap();
    let error = manager
        .start("session", &tool.id, &tool.version, &tool.package.sha256)
        .await
        .unwrap_err();
    assert_eq!(error.code, "platform_unverified");
    assert_eq!(downloads.load(Ordering::Relaxed), 0);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let progress =
        crate::ProgressReporter::new(tool.id.clone(), move |p| sink.lock().unwrap().push(p));
    let started = manager
        .start_with_options(
            "session",
            &tool.id,
            &tool.version,
            &tool.package.sha256,
            true,
            Some(progress),
        )
        .await
        .unwrap();
    let stages: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.stage.clone())
        .collect();
    assert_eq!(stages.first().unwrap(), "waiting");
    assert!(stages.contains(&"verify".into()));
    assert!(stages.contains(&"extract".into()));
    assert!(stages.contains(&"starting".into()));
    assert_eq!(stages.last().unwrap(), "ready");
    assert!(!manager.list_catalog().await.unwrap().1[0].compatible());
    manager.stop("session", &started.instance_id).await.unwrap();
}
