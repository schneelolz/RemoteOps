//! Local Codex onboarding helper. Secret input is restricted to files/stdin.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Args;
use rand::RngCore as _;
use remoteops_domain::ControllerInstanceId;
use remoteops_enrollment::{
    MAX_SETUP_BYTES, RedeemRequest, RedeemResponse, SetupDocument, validate_secret,
};
use remoteops_protocol::{
    ClientHello, ControllerHello, ControllerKind, PROTOCOL_VERSION, WireMessage, connect_tls,
    load_client_config, load_native_client_config, read_frame, write_frame,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

// CLI flags are mutually exclusive at parse/dispatch time.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Args)]
pub(super) struct SetupArgs {
    /// Import a .remoteops-setup file (the value is a path, never a secret).
    #[arg(long, conflicts_with = "setup_stdin")]
    setup_file: Option<PathBuf>,
    /// Read a setup code or JSON document from stdin, without command-line secrets.
    #[arg(long)]
    setup_stdin: bool,
    /// Validate and print only the destination preview; performs no network requests.
    #[arg(long, conflicts_with_all = ["setup_enroll", "check_credential", "remove_credential"])]
    setup_preview: bool,
    /// Redeem an explicitly confirmed setup and check the TLS Relay connection.
    #[arg(long, requires_all = ["setup_state", "setup_output"], conflicts_with_all = ["check_credential", "remove_credential"])]
    setup_enroll: bool,
    /// Durable non-secret retry checkpoint. Credentials stay in the OS credential store.
    #[arg(long)]
    setup_state: Option<PathBuf>,
    /// Non-secret connection configuration output path.
    #[arg(long)]
    setup_output: Option<PathBuf>,
    /// Check that this configuration's OS credential exists; does not print it.
    #[arg(long, conflicts_with = "remove_credential")]
    check_credential: bool,
    /// Remove only this configuration's local credential (does not revoke on Relay).
    #[arg(long)]
    remove_credential: bool,
    /// Validate Codex TOML without changing it.
    #[arg(long)]
    validate_codex: Option<PathBuf>,
    /// Inspect only known non-secret fields in the real parsed MCP table.
    #[arg(long)]
    inspect_codex: Option<PathBuf>,
    /// Configure only the `RemoteOps` MCP table.
    #[arg(long, requires_all = ["mcp_command", "mcp_config"])]
    configure_codex: Option<PathBuf>,
    /// Remove only the `RemoteOps` MCP table.
    #[arg(long)]
    unconfigure_codex: Option<PathBuf>,
    #[arg(long)]
    mcp_command: Option<String>,
    #[arg(long)]
    mcp_config: Option<String>,
    #[arg(long, default_value = "agent-controlled")]
    mcp_mode: String,
    #[arg(long)]
    legacy_env: bool,
}

impl SetupArgs {
    pub(super) fn requested(&self) -> bool {
        self.setup_file.is_some()
            || self.setup_stdin
            || self.setup_preview
            || self.setup_enroll
            || self.setup_state.is_some()
            || self.setup_output.is_some()
            || self.check_credential
            || self.remove_credential
            || self.validate_codex.is_some()
            || self.inspect_codex.is_some()
            || self.configure_codex.is_some()
            || self.unconfigure_codex.is_some()
    }
    fn run_codex_action(&self) -> anyhow::Result<bool> {
        let codex_actions = usize::from(self.validate_codex.is_some())
            + usize::from(self.inspect_codex.is_some())
            + usize::from(self.configure_codex.is_some())
            + usize::from(self.unconfigure_codex.is_some());
        if codex_actions > 0 {
            if codex_actions != 1
                || self.setup_file.is_some()
                || self.setup_stdin
                || self.setup_preview
                || self.setup_enroll
                || self.check_credential
                || self.remove_credential
            {
                bail!("Codex 配置操作不能与登记或凭据操作组合");
            }
            if let Some(path) = &self.validate_codex {
                super::codex_config::validate(path)?;
            }
            if let Some(path) = &self.inspect_codex {
                println!("{}", super::codex_config::inspect(path)?);
                return Ok(true);
            }
            if let Some(path) = &self.unconfigure_codex {
                super::codex_config::unconfigure(path)?;
            }
            if let Some(path) = &self.configure_codex {
                super::codex_config::configure(
                    path,
                    self.mcp_command.as_deref().context("缺少 MCP command")?,
                    self.mcp_config.as_deref().context("缺少 MCP config")?,
                    &self.mcp_mode,
                    self.legacy_env,
                )?;
            }
            println!("{{\"success\":true}}");
            return Ok(true);
        }
        Ok(false)
    }
    pub(super) async fn run(&self, config: Option<&Path>) -> anyhow::Result<()> {
        if self.run_codex_action()? {
            return Ok(());
        }
        let store = OsCredentialStore;
        if self.check_credential || self.remove_credential {
            if self.setup_file.is_some() || self.setup_stdin {
                bail!("凭据检查不接受安装码");
            }
            let path = config.context("凭据检查需要 --config")?;
            let configuration: super::McpFileConfig = serde_json::from_slice(&fs::read(path)?)?;
            let id = configuration
                .credential_id
                .context("配置未使用独立安装凭据")?;
            if self.remove_credential {
                store.remove(id)?;
            } else {
                let secret = store
                    .load(id)?
                    .context("未找到此安装的系统凭据，请重新运行安装器")?;
                validate_secret(&secret)?;
            }
            println!("{{\"success\":true}}");
            return Ok(());
        }
        if self.setup_preview == self.setup_enroll
            || (self.setup_file.is_some() == self.setup_stdin)
        {
            bail!("选择 --setup-file 或 --setup-stdin，并选择 --setup-preview 或 --setup-enroll");
        }
        let mut input = Zeroizing::new(String::new());
        let limit = u64::try_from(MAX_SETUP_BYTES + 1).unwrap_or(u64::MAX);
        if let Some(path) = &self.setup_file {
            File::open(path)?.take(limit).read_to_string(&mut input)?;
        } else {
            std::io::stdin()
                .lock()
                .take(limit)
                .read_to_string(&mut input)?;
        }
        let setup = SetupDocument::decode(&input)?;
        if self.setup_preview {
            println!(
                "{}",
                serde_json::json!({"relay":setup.relay,"server_name":setup.server_name,
                "enrollment_url":setup.enrollment_url,"expires_at":setup.expires_at,
                "uses_private_ca":setup.relay_ca_pem.is_some() || setup.enrollment_ca_pem.is_some()})
            );
            return Ok(());
        }
        let state_path = self.setup_state.as_deref().context("缺少恢复状态路径")?;
        let output_path = self.setup_output.as_deref().context("缺少连接配置路径")?;
        // OS lock is automatically released after a crash. Never delete a stale lock file.
        let _lock = lock_checkpoint(state_path)?;
        let (installation_id, secret) = prepare_installation(&setup, state_path, &store)?;
        let response = redeem(&setup, installation_id, &secret).await?;
        validate_response(&setup, installation_id, &response)?;
        publish_verified_config(
            &response,
            output_path,
            check_relay(&response, output_path, &secret),
        )
        .await
        .context("安装已登记，配置或连接自检尚未通过；保留恢复状态后重试，不要重新生成安装码")?;
        println!(
            "{}",
            serde_json::json!({"success":true,"relay":response.relay,"config":output_path,"credential_store":"os","connection_checked":true})
        );
        Ok(())
    }
}

trait CredentialStore {
    fn load(&self, id: Uuid) -> anyhow::Result<Option<Zeroizing<String>>>;
    fn save(&self, id: Uuid, secret: &str) -> anyhow::Result<()>;
    fn remove(&self, id: Uuid) -> anyhow::Result<()>;
}
struct OsCredentialStore;

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl OsCredentialStore {
    fn entry(id: Uuid) -> anyhow::Result<keyring::Entry> {
        keyring::Entry::store_status()
            .as_ref()
            .map_err(|_| anyhow::anyhow!("无法初始化系统凭据库"))?;
        #[cfg(target_os = "windows")]
        {
            // Explicitly local persistence: do not roam an installation secret
            // to another Windows computer via Enterprise credential roaming.
            let modifiers = std::collections::HashMap::from([("persistence", "Local")]);
            let inner = keyring_core::Entry::new_with_modifiers(
                "RemoteOps MCP Installation",
                &id.to_string(),
                &modifiers,
            )
            .context("无法打开系统凭据库")?;
            Ok(keyring::Entry { inner })
        }
        #[cfg(target_os = "macos")]
        {
            keyring::Entry::new("RemoteOps MCP Installation", &id.to_string())
                .context("无法打开系统凭据库")
        }
    }
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl CredentialStore for OsCredentialStore {
    fn load(&self, id: Uuid) -> anyhow::Result<Option<Zeroizing<String>>> {
        match Self::entry(id)?.get_password() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => bail!("无法读取系统安装凭据，请解锁凭据库后重试"),
        }
    }
    fn save(&self, id: Uuid, secret: &str) -> anyhow::Result<()> {
        Self::entry(id)?
            .set_password(secret)
            .map_err(|_| anyhow::anyhow!("无法保存系统安装凭据"))
    }
    fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => bail!("无法删除系统安装凭据"),
        }
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
impl CredentialStore for OsCredentialStore {
    fn load(&self, _: Uuid) -> anyhow::Result<Option<Zeroizing<String>>> {
        bail!("一次性安装仅支持 Windows 和 macOS 系统凭据库")
    }
    fn save(&self, _: Uuid, _: &str) -> anyhow::Result<()> {
        bail!("一次性安装仅支持 Windows 和 macOS 系统凭据库")
    }
    fn remove(&self, _: Uuid) -> anyhow::Result<()> {
        bail!("一次性安装仅支持 Windows 和 macOS 系统凭据库")
    }
}

pub(super) fn credential_token(id: Uuid) -> anyhow::Result<String> {
    let secret = OsCredentialStore
        .load(id)?
        .context("找不到此安装的系统凭据，请重新运行安装器")?;
    validate_secret(&secret)?;
    Ok(format!("roc1.{id}.{}", secret.as_str()))
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Checkpoints {
    installations: BTreeMap<Uuid, Checkpoint>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    installation_id: Uuid,
    setup_digest: String,
}

fn lock_checkpoint(path: &Path) -> anyhow::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock()
        .context("另一个安装正在使用此恢复状态，请稍后重试")?;
    Ok(lock)
}

fn prepare_installation(
    setup: &SetupDocument,
    path: &Path,
    store: &impl CredentialStore,
) -> anyhow::Result<(Uuid, Zeroizing<String>)> {
    let mut state: Checkpoints = match fs::read(path) {
        Ok(bytes) if bytes.len() <= MAX_SETUP_BYTES => {
            serde_json::from_slice(&bytes).context("安装恢复状态损坏；不要删除已领取安装的状态")?
        }
        Ok(_) => bail!("安装恢复状态过大"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Checkpoints::default(),
        Err(error) => return Err(error).context("无法读取安装恢复状态"),
    };
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(setup)?));
    if let Some(checkpoint) = state.installations.get(&setup.grant_id) {
        if checkpoint.setup_digest != digest {
            bail!("同一安装码的目标或信任配置已变化，拒绝重新发送凭据");
        }
        let secret = store
            .load(checkpoint.installation_id)?
            .context("恢复凭据不存在；请恢复系统凭据或在后台撤销旧安装后创建新的安装码")?;
        validate_secret(&secret)?;
        return Ok((checkpoint.installation_id, secret));
    }
    if state.installations.len() >= 256 {
        bail!("安装恢复记录已达到上限");
    }
    let installation_id = Uuid::new_v4();
    let mut bytes = Zeroizing::new([0_u8; 32]);
    rand::rngs::OsRng
        .try_fill_bytes(bytes.as_mut())
        .context("无法生成安全安装凭据")?;
    let secret = Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..]));
    // Store + verify BEFORE any network request, then durably checkpoint the reference.
    store.save(installation_id, &secret)?;
    let saved = store.load(installation_id)?.context("系统凭据保存未确认")?;
    if *saved != *secret {
        bail!("系统凭据保存校验失败");
    }
    state.installations.insert(
        setup.grant_id,
        Checkpoint {
            installation_id,
            setup_digest: digest,
        },
    );
    atomic_write(path, &serde_json::to_vec_pretty(&state)?)?;
    Ok((installation_id, secret))
}

fn enrollment_client(setup: &SetupDocument) -> anyhow::Result<reqwest::Client> {
    setup.validate()?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10));
    if let Some(pem) = &setup.enrollment_ca_pem {
        builder = builder.tls_built_in_root_certs(false);
        for certificate in
            reqwest::Certificate::from_pem_bundle(pem.as_bytes()).context("登记服务公开 CA 无效")?
        {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder.build().context("无法配置 HTTPS 登记连接")
}
async fn redeem(setup: &SetupDocument, id: Uuid, secret: &str) -> anyhow::Result<RedeemResponse> {
    let request = RedeemRequest {
        grant_id: setup.grant_id,
        grant_secret: setup.grant_secret.clone(),
        installation_id: id,
        installation_secret: secret.to_owned(),
    };
    let mut response = enrollment_client(setup)?
        .post(&setup.enrollment_url)
        .json(&request)
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!("HTTPS 登记连接失败；请检查地址、网络和证书，保留恢复状态后重试")
        })?;
    if !response.status().is_success() {
        // Never echo server bodies: they may contain attacker-controlled reflected credentials.
        bail!(
            "登记失败（HTTP {}）；请检查安装码状态，已领取安装请保留恢复状态重试",
            response.status().as_u16()
        );
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("登记响应中断；保留恢复状态后重试")?
    {
        if body.len() + chunk.len() > MAX_SETUP_BYTES {
            bail!("登记响应过大");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| anyhow::anyhow!("登记响应格式无效；保留恢复状态后重试"))
}
fn validate_response(
    setup: &SetupDocument,
    id: Uuid,
    response: &RedeemResponse,
) -> anyhow::Result<()> {
    if response.installation_id != id
        || response.owner_id.as_uuid().is_nil()
        || response.relay != setup.relay
        || response.server_name != setup.server_name
        || response.relay_ca_pem != setup.relay_ca_pem
    {
        bail!("登记响应与预览的安装目标不一致，拒绝使用");
    }
    Ok(())
}
async fn publish_verified_config(
    response: &RedeemResponse,
    path: &Path,
    verify: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let staged = path.with_file_name(format!(".remoteops-install-{}.json", Uuid::new_v4()));
    write_config(response, &staged)?;
    let result = async {
        verify.await?;
        fs::rename(&staged, path)?;
        #[cfg(unix)]
        File::open(path.parent().context("输出路径缺少父目录")?)?.sync_all()?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

fn write_config(response: &RedeemResponse, path: &Path) -> anyhow::Result<()> {
    let ca_cert = if let Some(pem) = &response.relay_ca_pem {
        let ca_path = path.with_file_name(format!("relay-ca-{}.pem", response.installation_id));
        atomic_write(&ca_path, pem.as_bytes())?;
        Some(fs::canonicalize(ca_path)?)
    } else {
        None
    };
    let config = serde_json::json!({"relay":response.relay,"server_name":response.server_name,
        "owner_id":response.owner_id,"reconnect_seconds":2,"credential_id":response.installation_id,"ca_cert":ca_cert});
    atomic_write(path, &serde_json::to_vec_pretty(&config)?)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("输出路径缺少父目录")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".remoteops-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
async fn check_relay(response: &RedeemResponse, path: &Path, secret: &str) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let tls = if response.relay_ca_pem.is_some() {
            load_client_config(
                path.with_file_name(format!("relay-ca-{}.pem", response.installation_id)),
            )?
        } else {
            load_native_client_config()?
        };
        let mut stream = connect_tls(&response.relay, &response.server_name, tls).await?;
        write_frame(
            &mut stream,
            &WireMessage::Hello(ClientHello::Controller(ControllerHello {
                protocol_version: PROTOCOL_VERSION,
                controller_instance_id: ControllerInstanceId::new(),
                owner_id: response.owner_id,
                kind: ControllerKind::Ai,
                auth_token: format!("roc1.{}.{}", response.installation_id, secret),
                hostname: None,
                mac_address: None,
            })),
        )
        .await?;
        match read_frame::<WireMessage, _>(&mut stream).await? {
            WireMessage::ControllerWelcome {
                protocol_version: PROTOCOL_VERSION,
            } => Ok(()),
            _ => bail!("Relay 未确认此安装的 AI Controller 身份"),
        }
    })
    .await
    .context("Relay 连接自检超时")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[derive(Default)]
    struct MemoryStore(Mutex<BTreeMap<Uuid, String>>);
    impl CredentialStore for MemoryStore {
        fn load(&self, id: Uuid) -> anyhow::Result<Option<Zeroizing<String>>> {
            Ok(self.0.lock().unwrap().get(&id).cloned().map(Zeroizing::new))
        }
        fn save(&self, id: Uuid, secret: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().insert(id, secret.to_owned());
            Ok(())
        }
        fn remove(&self, id: Uuid) -> anyhow::Result<()> {
            self.0.lock().unwrap().remove(&id);
            Ok(())
        }
    }
    fn setup() -> SetupDocument {
        SetupDocument {
            version: 1,
            grant_id: Uuid::new_v4(),
            grant_secret: URL_SAFE_NO_PAD.encode([2; 32]),
            enrollment_url: "https://example.test/api/mcp/enroll".into(),
            relay: "example.test:7443".into(),
            server_name: "example.test".into(),
            relay_ca_pem: None,
            enrollment_ca_pem: None,
            expires_at: chrono::Utc::now() - chrono::Duration::days(1),
        }
    }
    fn temp_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("remoteops-setup-test-{}", Uuid::new_v4()))
            .join("state.json")
    }
    #[test]
    fn retry_preserves_secret_after_expiry_and_no_secrets_on_disk() {
        let path = temp_path();
        let store = MemoryStore::default();
        let setup = setup();
        let (id, secret) = prepare_installation(&setup, &path, &store).unwrap();
        let (same_id, same_secret) = prepare_installation(&setup, &path, &store).unwrap();
        assert_eq!(id, same_id);
        assert_eq!(*secret, *same_secret);
        let state = fs::read_to_string(&path).unwrap();
        assert!(!state.contains(secret.as_str()));
        assert!(!state.contains(&setup.grant_secret));
        let mut changed = setup.clone();
        changed.enrollment_url = "https://other.test/api/mcp/enroll".into();
        assert!(prepare_installation(&changed, &path, &store).is_err());
        store.remove(id).unwrap();
        assert!(prepare_installation(&setup, &path, &store).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn checkpoint_lock_blocks_parallel_installation_and_survives_release() {
        let path = temp_path();
        let first = lock_checkpoint(&path).unwrap();
        assert!(lock_checkpoint(&path).is_err());
        drop(first);
        assert!(lock_checkpoint(&path).is_ok());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn configuration_and_response_are_bound_to_preview() {
        let setup = setup();
        let id = Uuid::new_v4();
        let mut response = RedeemResponse {
            installation_id: id,
            owner_id: remoteops_domain::ControllerOwnerId::new(),
            relay: setup.relay.clone(),
            server_name: setup.server_name.clone(),
            relay_ca_pem: None,
        };
        assert!(validate_response(&setup, id, &response).is_ok());
        response.relay = "other.test:7443".into();
        assert!(validate_response(&setup, id, &response).is_err());
        response.relay = setup.relay;
        let path = temp_path();
        write_config(&response, &path).unwrap();
        let config: super::super::McpFileConfig =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config.credential_id, Some(id));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn malformed_ca_and_insecure_endpoint_rejected_before_send() {
        let mut input = setup();
        input.enrollment_url = "http://example.test/api/mcp/enroll".into();
        assert!(enrollment_client(&input).is_err());
        input = setup();
        input.enrollment_ca_pem = Some("not a certificate".into());
        assert!(enrollment_client(&input).is_err());
    }
    async fn https_fixture(
        response: String,
    ) -> (SetupDocument, tokio::task::JoinHandle<Option<String>>) {
        use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate.cert.der().to_vec())],
                PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut setup = setup();
        setup.enrollment_url = format!(
            "https://localhost:{}/api/mcp/enroll",
            listener.local_addr().unwrap().port()
        );
        setup.enrollment_ca_pem = Some(certificate.cert.pem());
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = tokio_rustls::TlsAcceptor::from(Arc::new(tls))
                .accept(stream)
                .await
            else {
                return None;
            };
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    return None;
                }
                request.extend_from_slice(&buffer[..count]);
                assert!(request.len() < MAX_SETUP_BYTES);
                let text = String::from_utf8_lossy(&request);
                if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if body.len() >= length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            Some(String::from_utf8(request).unwrap())
        });
        (setup, task)
    }
    #[tokio::test]
    async fn https_rejects_untrusted_certificates_before_sending_credentials() {
        let (mut setup, task) =
            https_fixture("HTTP/1.1 500 Error\r\nContent-Length: 0\r\n\r\n".into()).await;
        setup.enrollment_ca_pem = None;
        assert!(
            redeem(&setup, Uuid::new_v4(), &URL_SAFE_NO_PAD.encode([1; 32]))
                .await
                .is_err()
        );
        assert!(task.await.unwrap().is_none());
    }
    #[tokio::test]
    async fn https_keeps_secrets_in_post_body_and_rejects_redirects() {
        let (setup,task)=https_fixture("HTTP/1.1 302 Found\r\nLocation: https://attacker.invalid/collect\r\nContent-Length: 0\r\n\r\n".into()).await;
        let secret = URL_SAFE_NO_PAD.encode([1; 32]);
        let error = redeem(&setup, Uuid::new_v4(), &secret)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("302"));
        assert!(!error.contains(&secret));
        assert!(!error.contains(&setup.grant_secret));
        let request = task.await.unwrap().unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /api/mcp/enroll HTTP/1.1"));
        assert!(!headers.contains(&secret));
        assert!(!headers.contains(&setup.grant_secret));
        let payload: RedeemRequest = serde_json::from_str(body).unwrap();
        assert_eq!(payload.installation_secret, secret);
        assert_eq!(payload.grant_secret, setup.grant_secret);
    }
    #[tokio::test]
    async fn failed_replacement_preserves_live_configuration_and_retry_succeeds() {
        let path = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"existing working configuration";
        fs::write(&path, original).unwrap();
        let setup = setup();
        let response = RedeemResponse {
            installation_id: Uuid::new_v4(),
            owner_id: remoteops_domain::ControllerOwnerId::new(),
            relay: setup.relay,
            server_name: setup.server_name,
            relay_ca_pem: None,
        };
        assert!(
            publish_verified_config(&response, &path, async { bail!("test self-check failure") })
                .await
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        publish_verified_config(&response, &path, async { Ok(()) })
            .await
            .unwrap();
        let config: super::super::McpFileConfig =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config.credential_id, Some(response.installation_id));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn relay_self_check_sends_only_enrolled_ai_hello() {
        use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
        use std::sync::Arc;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate.cert.der().to_vec())],
                PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let path = temp_path();
        let secret = URL_SAFE_NO_PAD.encode([3; 32]);
        let response = RedeemResponse {
            installation_id: Uuid::new_v4(),
            owner_id: remoteops_domain::ControllerOwnerId::new(),
            relay: listener.local_addr().unwrap().to_string(),
            server_name: "localhost".into(),
            relay_ca_pem: Some(certificate.cert.pem()),
        };
        let expected_owner = response.owner_id;
        let expected_token = format!("roc1.{}.{}", response.installation_id, secret);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(tls))
                .accept(stream)
                .await
                .unwrap();
            let WireMessage::Hello(ClientHello::Controller(hello)) =
                read_frame::<WireMessage, _>(&mut stream).await.unwrap()
            else {
                panic!("expected Controller hello")
            };
            assert_eq!(hello.owner_id, expected_owner);
            assert_eq!(hello.kind, ControllerKind::Ai);
            assert_eq!(hello.auth_token, expected_token);
            write_frame(
                &mut stream,
                &WireMessage::ControllerWelcome {
                    protocol_version: PROTOCOL_VERSION,
                },
            )
            .await
            .unwrap();
            assert!(read_frame::<WireMessage, _>(&mut stream).await.is_err());
        });
        publish_verified_config(&response, &path, check_relay(&response, &path, &secret))
            .await
            .unwrap();
        server.await.unwrap();
        assert!(path.is_file());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
