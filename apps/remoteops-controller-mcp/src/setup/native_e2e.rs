//! Opt-in native onboarding against a real, isolated Relay process.
//! The fixture compiles on every host; only Windows/macOS run the OS-store test.
#![cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]

use super::*;
use anyhow::ensure;
use remoteops_domain::ControllerOwnerId;
use remoteops_enrollment::{
    AdvertisedEndpoints, ClientSummary, CreateGrantRequest, CreateGrantResponse, GrantSummary,
};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{
    net::{SocketAddr, TcpListener as PortReservation},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

struct NativeFixture {
    root: PathBuf,
    owns_root: bool,
    relay: Option<Child>,
    proxy: Option<JoinHandle<()>>,
    credentials: Mutex<Vec<Uuid>>,
    admin_url: String,
    admin_token: String,
    log_secrets: Vec<String>,
    owner_id: ControllerOwnerId,
    endpoints: AdvertisedEndpoints,
    http: reqwest::Client,
    armed: bool,
}

/// Track ownership before the first OS write, including save/checkpoint failures.
struct TrackedNativeStore<'a>(&'a Mutex<Vec<Uuid>>);

impl CredentialStore for TrackedNativeStore<'_> {
    fn load(&self, id: Uuid) -> anyhow::Result<Option<Zeroizing<String>>> {
        OsCredentialStore.load(id)
    }

    fn save(&self, id: Uuid, secret: &str) -> anyhow::Result<()> {
        ensure!(
            OsCredentialStore.load(id)?.is_none(),
            "random native test credential identifier already exists"
        );
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("test cleanup ownership lock poisoned"))?
            .push(id);
        OsCredentialStore.save(id, secret)
    }

    fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        ensure!(
            self.0
                .lock()
                .map_err(|_| anyhow::anyhow!("test cleanup ownership lock poisoned"))?
                .contains(&id),
            "test cannot delete an unowned credential"
        );
        OsCredentialStore.remove(id)
    }
}

impl NativeFixture {
    #[allow(clippy::too_many_lines)]
    async fn start() -> anyhow::Result<Self> {
        let executable = fs::canonicalize(std::env::var_os("REMOTEOPS_TEST_RELAY").context(
            "set REMOTEOPS_TEST_RELAY to an isolated, locally built remoteops-relay executable",
        )?)?;
        ensure!(
            executable.is_file(),
            "REMOTEOPS_TEST_RELAY must name a file"
        );
        let root =
            std::env::temp_dir().join(format!("remoteops-native-enrollment-{}", Uuid::new_v4()));
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut fixture = Self {
            root,
            owns_root: false,
            relay: None,
            proxy: None,
            credentials: Mutex::new(Vec::new()),
            admin_url: String::new(),
            admin_token: format!("isolated-test-admin-{}", Uuid::new_v4()),
            log_secrets: Vec::new(),
            owner_id: ControllerOwnerId::new(),
            endpoints: AdvertisedEndpoints {
                enrollment_url: String::new(),
                relay: String::new(),
                server_name: "localhost".to_owned(),
                relay_ca_pem: None,
                enrollment_ca_pem: None,
            },
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()?,
            armed: true,
        };
        fs::create_dir(&fixture.root)?;
        fixture.owns_root = true;
        // macOS temp paths commonly traverse /var -> /private/var. Keep SQLite's
        // production NOFOLLOW policy intact by using this owned directory's real path.
        #[cfg(unix)]
        {
            fixture.root = fs::canonicalize(&fixture.root)?;
        }
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        let certificate_path = fixture.root.join("relay-cert.pem");
        let key_path = fixture.root.join("relay-key.pem");
        atomic_write(&certificate_path, certificate.cert.pem().as_bytes())?;
        atomic_write(
            &key_path,
            certificate.signing_key.serialize_pem().as_bytes(),
        )?;

        // Reserve distinct loopback ports together, then release immediately before spawning.
        // Relay's CLI does not support adopting pre-bound sockets.
        let business = PortReservation::bind("127.0.0.1:0")?;
        let admin = PortReservation::bind("127.0.0.1:0")?;
        let health = PortReservation::bind("127.0.0.1:0")?;
        let business_address = business.local_addr()?;
        let admin_address = admin.local_addr()?;
        let health_address = health.local_addr()?;
        fixture.admin_url = format!("http://{admin_address}");
        fixture.endpoints.relay = business_address.to_string();
        fixture.endpoints.relay_ca_pem = Some(certificate.cert.pem());
        fixture.endpoints.enrollment_ca_pem = Some(certificate.cert.pem());

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        fixture.endpoints.enrollment_url =
            format!("https://{}/api/mcp/enroll", listener.local_addr()?);
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate.cert.der().to_vec())],
                PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
            )?;
        fixture.proxy = Some(tokio::spawn(https_passthrough(
            listener,
            tls,
            admin_address,
        )));

        let human_token = format!("isolated-test-human-{}", Uuid::new_v4());
        let ai_token = format!("isolated-test-ai-{}", Uuid::new_v4());
        fixture.log_secrets = vec![
            fixture.admin_token.clone(),
            human_token.clone(),
            ai_token.clone(),
        ];
        let mut stderr_options = OpenOptions::new();
        stderr_options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            stderr_options.mode(0o600);
        }
        let stderr_file = stderr_options.open(fixture.root.join("relay-stderr.log"))?;
        let mut command = Command::new(executable);
        // Never inherit an operator's Relay configuration or credentials into this test child.
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("REMOTEOPS_")
            {
                command.env_remove(name);
            }
        }
        command
            .current_dir(&fixture.root)
            .env("REMOTEOPS_RELAY_BIND", business_address.to_string())
            .env("REMOTEOPS_ADMIN_ADDR", admin_address.to_string())
            .env("REMOTEOPS_HEALTH_BIND", health_address.to_string())
            .env("REMOTEOPS_TLS_CERT", &certificate_path)
            .env("REMOTEOPS_TLS_KEY", &key_path)
            .env(
                "REMOTEOPS_STATE_FILE",
                fixture.root.join("relay-state.json"),
            )
            .env(
                "REMOTEOPS_CONTROLLER_OWNER_ID",
                fixture.owner_id.to_string(),
            )
            .env("REMOTEOPS_ADMIN_TOKEN", &fixture.admin_token)
            .env("REMOTEOPS_HUMAN_CONTROLLER_TOKEN", &human_token)
            .env("REMOTEOPS_AI_CONTROLLER_TOKEN", &ai_token)
            .env("RUST_LOG", "remoteops_relay=info")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file));
        drop((business, admin, health));
        fixture.relay = Some(
            command
                .spawn()
                .context("could not start isolated Relay fixture")?,
        );
        fixture.wait_ready().await?;
        Ok(fixture)
    }

    async fn wait_ready(&mut self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self
                .relay
                .as_mut()
                .context("Relay child missing")?
                .try_wait()?
            {
                bail!(
                    "isolated Relay exited before readiness: {status}; stderr: {}",
                    self.stderr_summary()
                );
            }
            if let Ok(response) = self
                .http
                .get(format!("{}/api/admin/mcp/settings", self.admin_url))
                .bearer_auth(&self.admin_token)
                .send()
                .await
                && response.status().is_success()
            {
                return Ok(());
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "isolated Relay readiness timed out; stderr: {}",
                self.stderr_summary()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn stderr_summary(&self) -> String {
        let read = (|| -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(self.root.join("relay-stderr.log"))?
                .take(8193)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match read {
            Ok(bytes) => sanitize_relay_stderr(&bytes, &self.log_secrets),
            Err(_) => "<Relay stderr unavailable>".to_owned(),
        }
    }

    async fn clients(&self) -> anyhow::Result<Vec<ClientSummary>> {
        Ok(self
            .http
            .get(format!("{}/api/admin/mcp/clients", self.admin_url))
            .bearer_auth(&self.admin_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn mint_setup(&self) -> anyhow::Result<CreateGrantResponse> {
        self.http
            .put(format!("{}/api/admin/mcp/settings", self.admin_url))
            .bearer_auth(&self.admin_token)
            .json(&self.endpoints)
            .send()
            .await?
            .error_for_status()?;
        let minted = self
            .http
            .post(format!("{}/api/admin/mcp/setups", self.admin_url))
            .bearer_auth(&self.admin_token)
            .json(&CreateGrantRequest {
                client_name: Some("isolated native onboarding test".to_owned()),
                expires_in_hours: Some(1),
            })
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            minted
                .headers()
                .get(reqwest::header::CACHE_CONTROL)
                .is_some_and(|value| value == "no-store"),
            "real mint response must not be cached"
        );
        let grant: CreateGrantResponse = minted.json().await?;
        grant.setup.validate()?;
        ensure!(
            grant.setup.endpoints() == self.endpoints,
            "real grant destinations changed"
        );
        Ok(grant)
    }

    #[allow(clippy::too_many_lines)]
    async fn exercise(&self) -> anyhow::Result<()> {
        let grant = self.mint_setup().await?;
        let setup_path = self.root.join("test.remoteops-setup");
        let checkpoint_path = self.root.join("checkpoint.json");
        let config_path = self.root.join("mcp.json");
        atomic_write(&setup_path, &serde_json::to_vec(&grant.setup)?)?;

        let store = TrackedNativeStore(&self.credentials);
        let (installation_id, secret) =
            prepare_installation(&grant.setup, &checkpoint_path, &store)?;
        ensure!(
            self.credentials
                .lock()
                .map_err(|_| anyhow::anyhow!("test cleanup lock poisoned"))?
                .as_slice()
                == [installation_id],
            "test must own exactly one credential"
        );
        let saved = OsCredentialStore
            .load(installation_id)?
            .context("native credential missing after production prepare_installation")?;
        ensure!(
            saved.as_str() == secret.as_str(),
            "native installation credential mismatch"
        );
        let args = SetupArgs {
            setup_file: Some(setup_path),
            setup_enroll: true,
            setup_state: Some(checkpoint_path.clone()),
            setup_output: Some(config_path.clone()),
            ..SetupArgs::default()
        };
        // This is the production path: HTTPS redemption, OS-store recovery, Relay TLS Hello,
        // ControllerWelcome validation, and only then publication of the real config.
        args.run(None).await?;
        let config_bytes = fs::read(&config_path)?;
        let config: super::super::McpFileConfig = serde_json::from_slice(&config_bytes)?;
        ensure!(
            config.credential_id == Some(installation_id),
            "configuration credential reference changed"
        );
        ensure!(
            config.owner_id == Some(self.owner_id),
            "configuration owner was not server assigned"
        );
        ensure!(
            config.relay.as_deref() == Some(self.endpoints.relay.as_str()),
            "configuration Relay destination changed"
        );
        ensure!(
            config.server_name.as_deref() == Some("localhost"),
            "configuration TLS server name changed"
        );
        ensure!(
            config.ca_cert.as_ref().is_some_and(|path| path.is_file()),
            "private test trust was not installed"
        );
        for bytes in [&config_bytes, &fs::read(&checkpoint_path)?] {
            let text = std::str::from_utf8(bytes)?;
            ensure!(
                !text.contains(secret.as_str()) && !text.contains(&grant.setup.grant_secret),
                "secret leaked into ordinary installation metadata"
            );
        }
        let clients = self.clients().await?;
        ensure!(
            clients.len() == 1
                && clients[0].installation_id == installation_id
                && !clients[0].revoked
                && clients[0].last_seen.is_some(),
            "real Relay did not authenticate the enrolled client"
        );
        let grants: Vec<GrantSummary> = self
            .http
            .get(format!("{}/api/admin/mcp/setups", self.admin_url))
            .bearer_auth(&self.admin_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure!(
            grants.len() == 1
                && grants[0].state == "redeemed"
                && grants[0].installation_id == Some(installation_id),
            "real grant was not atomically consumed"
        );

        // Re-run the unchanged installer checkpoint after a successful response. The real
        // backend must return the same identity and must not create a second installation.
        args.run(None).await?;
        ensure!(
            self.clients().await?.len() == 1,
            "setup retry created a second client"
        );
        ensure!(
            fs::read(&config_path)? == config_bytes,
            "setup retry changed the verified configuration"
        );
        SetupArgs {
            check_credential: true,
            ..SetupArgs::default()
        }
        .run(Some(&config_path))
        .await?;
        self.http
            .post(format!(
                "{}/api/admin/mcp/clients/{installation_id}/revoke",
                self.admin_url
            ))
            .bearer_auth(&self.admin_token)
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            self.clients()
                .await?
                .first()
                .is_some_and(|client| client.revoked),
            "real Relay did not persist client revocation"
        );
        let revoked_error = args
            .run(None)
            .await
            .err()
            .context("revoked client resumed enrollment")?;
        ensure!(
            revoked_error.to_string().contains("HTTP 401"),
            "revoked retry must fail with the real backend credential rejection"
        );
        ensure!(
            fs::read(&config_path)? == config_bytes,
            "failed revoked retry replaced the prior configuration"
        );
        let response = RedeemResponse {
            installation_id,
            owner_id: self.owner_id,
            relay: self.endpoints.relay.clone(),
            server_name: self.endpoints.server_name.clone(),
            relay_ca_pem: self.endpoints.relay_ca_pem.clone(),
        };
        ensure!(
            check_relay(&response, &config_path, &secret).await.is_err(),
            "revoked credential still authenticated to the real TLS Relay"
        );
        ensure!(
            self.clients()
                .await?
                .first()
                .is_some_and(|client| client.installation_id == installation_id && client.revoked),
            "Relay must remain healthy after rejecting the revoked TLS credential"
        );
        SetupArgs {
            remove_credential: true,
            ..SetupArgs::default()
        }
        .run(Some(&config_path))
        .await?;
        ensure!(
            OsCredentialStore.load(installation_id)?.is_none(),
            "production credential deletion left the dummy credential behind"
        );
        Ok(())
    }

    async fn cleanup(&mut self) -> anyhow::Result<()> {
        if let Some(proxy) = self.proxy.take() {
            proxy.abort();
            let _ = proxy.await;
        }
        self.cleanup_sync()
    }

    fn cleanup_sync(&mut self) -> anyhow::Result<()> {
        let mut failures = Vec::new();
        if let Some(proxy) = self.proxy.take() {
            proxy.abort();
        }
        if let Some(mut child) = self.relay.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) && child.kill().is_err() {
                failures.push("could not terminate isolated Relay");
            }
            if child.wait().is_err() {
                failures.push("could not reap isolated Relay");
            }
        }
        match self.credentials.lock() {
            Ok(ids) => {
                for id in ids.iter().copied() {
                    let removed = OsCredentialStore.remove(id).and_then(|()| {
                        ensure!(
                            OsCredentialStore.load(id)?.is_none(),
                            "test credential remained after deletion"
                        );
                        Ok(())
                    });
                    if removed.is_err() {
                        eprintln!(
                            "native onboarding credential cleanup failed for installation {id}"
                        );
                        failures.push("could not remove isolated native credential");
                    }
                }
            }
            Err(_) => failures.push("test cleanup ownership lock poisoned"),
        }
        if self.owns_root && self.root.exists() && fs::remove_dir_all(&self.root).is_err() {
            failures.push("could not remove isolated test directory");
        }
        self.armed = !failures.is_empty();
        ensure!(failures.is_empty(), "{}", failures.join("; "));
        Ok(())
    }
}

/// Only report a bounded, redacted diagnostic; the owned raw file is deleted with the fixture.
fn sanitize_relay_stderr(bytes: &[u8], secrets: &[String]) -> String {
    let input_truncated = bytes.len() > 8192;
    let mut bounded = &bytes[..bytes.len().min(8192)];
    if input_truncated {
        // Do not echo a secret fragment if the byte cap cuts through the final log line.
        bounded = bounded
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(&[][..], |last| &bounded[..=last]);
    }
    let text = String::from_utf8_lossy(bounded);
    let mut inside_key = false;
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            inside_key = true;
            lines.push("[REDACTED PRIVATE KEY]".to_owned());
        }
        if inside_key {
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                inside_key = false;
            }
            continue;
        }
        let mut line: String = line
            .chars()
            .filter(|character| !character.is_control() || *character == '\t')
            .collect();
        for secret in secrets {
            if !secret.is_empty() {
                line = line.replace(secret, "[REDACTED]");
            }
        }
        lines.push(
            line.split_whitespace()
                .map(|word| {
                    if word.contains("roc1.") || word.contains("remoteops-setup-v1.") {
                        "[REDACTED]"
                    } else {
                        word
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let mut summary = lines.join("\n");
    let output_truncated = summary.len() > 4096;
    if output_truncated {
        let cut = summary
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= 4096)
            .last()
            .unwrap_or(0);
        summary.truncate(cut);
    }
    if input_truncated || output_truncated {
        summary.push_str("\n[stderr truncated]");
    }
    if summary.trim().is_empty() {
        "<no Relay stderr output>".to_owned()
    } else {
        summary
    }
}

#[test]
fn relay_startup_diagnostics_redact_credentials_and_private_keys() {
    let secret = "isolated-test-admin-do-not-echo".to_owned();
    let input = format!(
        "Error: SQLite refused symlink\nTOKEN={secret}\nroc1.abc.secret remoteops-setup-v1.secret\n-----BEGIN PRIVATE KEY-----\nprivate-key-body\n-----END PRIVATE KEY-----\nready\u{001b}"
    );
    let result = sanitize_relay_stderr(input.as_bytes(), std::slice::from_ref(&secret));
    assert!(result.contains("SQLite refused symlink"));
    assert!(result.contains("[REDACTED]"));
    assert!(!result.contains(&secret));
    assert!(!result.contains("roc1."));
    assert!(!result.contains("remoteops-setup-v1."));
    assert!(!result.contains("private-key-body"));
    assert!(!result.contains('\u{001b}'));
}

#[test]
fn relay_startup_diagnostics_bound_output_and_drop_partial_lines() {
    let result = sanitize_relay_stderr(
        format!("visible\n{}", "sensitive-tail".repeat(1000)).as_bytes(),
        &[],
    );
    assert_eq!(result, "visible\n[stderr truncated]");
    let result = sanitize_relay_stderr("long diagnostic line\n".repeat(300).as_bytes(), &[]);
    assert!(result.len() <= 4096 + "\n[stderr truncated]".len());
}

impl Drop for NativeFixture {
    fn drop(&mut self) {
        assert!(
            !(self.armed && self.cleanup_sync().is_err() && !std::thread::panicking()),
            "native onboarding fixture cleanup failed"
        );
    }
}

/// TLS terminates locally and every HTTP byte is forwarded to the real Relay admin listener.
/// No enrollment response, credential validation, or Relay Hello is mocked.
async fn https_passthrough(listener: TcpListener, tls: rustls::ServerConfig, backend: SocketAddr) {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else { break; };
                let acceptor = acceptor.clone();
                connections.spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(30), async {
                        let mut front = acceptor.accept(socket).await?;
                        let mut back = TcpStream::connect(backend).await?;
                        tokio::io::copy_bidirectional(&mut front, &mut back).await
                    }).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[tokio::test]
#[ignore = "runs a local Relay and creates/deletes one isolated native OS credential"]
async fn native_os_store_real_relay_onboarding() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let previous_interaction =
        security_framework::os::macos::keychain::SecKeychain::user_interaction_allowed()?;
    #[cfg(target_os = "macos")]
    let interaction_guard = if previous_interaction {
        Some(security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()?)
    } else {
        None
    };

    let mut fixture = NativeFixture::start().await?;
    let outcome = tokio::time::timeout(Duration::from_secs(120), fixture.exercise())
        .await
        .context("native onboarding exceeded its bounded verification window");
    let cleanup = fixture.cleanup().await;
    // Any cleanup retry must happen while Keychain interaction remains disabled.
    drop(fixture);
    #[cfg(target_os = "macos")]
    {
        drop(interaction_guard);
        ensure!(
            security_framework::os::macos::keychain::SecKeychain::user_interaction_allowed()?
                == previous_interaction,
            "process-local Keychain interaction state was not restored"
        );
    }
    cleanup?;
    outcome?
}

/// Host-independent transport verification deliberately makes no OS-store claim.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
#[tokio::test]
#[ignore = "runs an isolated local Relay transport fixture; requires REMOTEOPS_TEST_RELAY"]
async fn loopback_real_relay_transport_smoke() -> anyhow::Result<()> {
    let mut fixture = NativeFixture::start().await?;
    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        let grant = fixture.mint_setup().await?;
        let id = Uuid::new_v4();
        let secret = Zeroizing::new(URL_SAFE_NO_PAD.encode([73_u8; 32]));
        let response = redeem(&grant.setup, id, &secret).await?;
        validate_response(&grant.setup, id, &response)?;
        let path = fixture.root.join("transport-smoke.json");
        publish_verified_config(&response, &path, check_relay(&response, &path, &secret)).await?;
        ensure!(
            fixture.clients().await?.len() == 1,
            "real Relay client is missing"
        );
        let retry = redeem(&grant.setup, id, &secret).await?;
        ensure!(
            retry.installation_id == id && fixture.clients().await?.len() == 1,
            "real grant retry changed installation"
        );
        fixture
            .http
            .post(format!(
                "{}/api/admin/mcp/clients/{id}/revoke",
                fixture.admin_url
            ))
            .bearer_auth(&fixture.admin_token)
            .send()
            .await?
            .error_for_status()?;
        let revoked_error = redeem(&grant.setup, id, &secret)
            .await
            .err()
            .context("real revoked grant retry was accepted")?;
        ensure!(
            revoked_error.to_string().contains("HTTP 401"),
            "real revoked retry did not return credential rejection"
        );
        ensure!(
            check_relay(&response, &path, &secret).await.is_err(),
            "real revoked TLS credential was accepted"
        );
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("loopback transport smoke timed out");
    let cleanup = fixture.cleanup().await;
    drop(fixture);
    cleanup?;
    outcome?
}
