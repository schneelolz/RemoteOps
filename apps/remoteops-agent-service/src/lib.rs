//! 可复用的服务宿主；定制产品仅提供资料与约束，无需复制 SCM、停止或状态管理。
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(windows)]
use std::ffi::OsString;
use std::{fs, io::Write, sync::mpsc};

use anyhow::Context;
use chrono::{DateTime, Utc};
use clap::Parser;
use remoteops_agent::AgentEvent;
use remoteops_agent::{
    AgentCapabilityPolicy, AgentConfig, AgentPermissionControl, AgentRunMode, AgentRuntimeOptions,
    ProductProfile, run_agent_with_options,
};
use remoteops_domain::AgentInstanceId;
use serde::Serialize;
use tokio::sync::watch;

#[cfg(windows)]
const DEFAULT_CONFIG: &str = r"C:\ProgramData\RemoteOps\Agent\agent-config.json";
#[cfg(not(windows))]
const DEFAULT_CONFIG: &str = "/etc/remoteops/agent-config.json";
#[cfg(windows)]
const DEFAULT_STATUS: &str = r"C:\ProgramData\RemoteOps\Agent\runtime-status.json";
#[cfg(not(windows))]
const DEFAULT_STATUS: &str = "/run/remoteops-agent/runtime-status.json";

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = interrupt.recv() => {},
        _ = terminate.recv() => {},
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

/// `RemoteOps` Agent 服务宿主参数。
#[derive(Debug, Parser)]
#[command(version, about = "RemoteOps Agent service host")]
struct Args {
    /// Agent 配置文件；服务安装脚本会写入机器级目录。
    #[arg(long)]
    config: Option<PathBuf>,
    /// 服务运行状态文件；停止服务后自动删除。
    #[arg(long)]
    status_file: Option<PathBuf>,
    /// 在前台运行，用于安装前验证配置，不注册 SCM 服务。
    #[arg(long)]
    console: bool,
    /// Validate config, TLS files and writable directories without connecting.
    #[arg(long)]
    check_config: bool,
}

/// 服务宿主定制选项，构建器只提供宿主资料与收紧后的能力。
#[derive(Clone)]
pub struct ServiceOptions {
    service_name: String,
    config_file: Option<PathBuf>,
    status_file: Option<PathBuf>,
    runtime: AgentRuntimeOptions,
}
impl Default for ServiceOptions {
    fn default() -> Self {
        Self::new("RemoteOpsAgent")
    }
}
impl ServiceOptions {
    /// 设置 SCM 内部服务名。
    #[must_use]
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            config_file: None,
            status_file: None,
            runtime: AgentRuntimeOptions::service(ProductProfile::default()),
        }
    }
    /// 覆盖机器级配置与状态文件路径。
    #[must_use]
    pub fn with_paths(mut self, config: PathBuf, status: PathBuf) -> Self {
        self.config_file = Some(config);
        self.status_file = Some(status);
        self
    }
    /// 将服务配置为门户受管目标，必须另行部署登记凭据文件。
    #[must_use]
    pub fn managed(mut self) -> Self {
        self.runtime.pairing_mode = remoteops_protocol::PairingMode::ManagedOnDemand;
        self
    }
    /// 在服务无图形上限之上继续限制为只读。
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.runtime.capability_policy = AgentCapabilityPolicy::headless().with_read_only();
        self
    }
    /// 为当前目标附加产品操作指引。
    #[must_use]
    pub fn with_guidance(mut self, guidance: Vec<remoteops_domain::TargetGuidance>) -> Self {
        self.runtime.guidance = guidance;
        self
    }
}

#[derive(Clone)]
struct Launch {
    profile: ProductProfile,
    options: ServiceOptions,
    args: ResolvedArgs,
}
#[derive(Clone)]
struct ResolvedArgs {
    config: PathBuf,
    status_file: PathBuf,
    console: bool,
    check_config: bool,
}

fn runtime_options(launch: &Launch, mode: AgentRunMode) -> AgentRuntimeOptions {
    let mut options = launch.options.runtime.clone();
    options.product = launch.profile.clone();
    options.run_mode = mode;
    options.visual_enabled = false;
    options
}

/// 仅供本机服务管理使用的短期状态。
#[derive(Debug, Default, Serialize)]
struct RuntimeStatus {
    /// 当前服务状态。
    status: String,
    /// Agent 实例标识。
    agent_instance_id: Option<AgentInstanceId>,
    /// Relay 地址；不包含凭据。
    relay: Option<String>,
    /// 当前临时控制码。
    pairing_code: Option<String>,
    /// 控制码过期时间。
    pairing_code_expires_at: Option<DateTime<Utc>>,
    /// 当前控制端数量。
    active_connections: usize,
}

fn load_config(path: &Path, profile: &ProductProfile) -> anyhow::Result<AgentConfig> {
    let config = AgentConfig::load_service_file_with_profile(path, profile)?
        .resolve_product(profile, None)?;
    config.normalize_and_validate()
}

fn write_status(path: &Path, status: &RuntimeStatus) {
    let result = (|| -> anyhow::Result<()> {
        let contents = serde_json::to_vec_pretty(status)?;
        let parent = path.parent().context("Status path has no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // The service owns its runtime directory. Recover a leftover temp file
        // after a previous process was killed while publishing status.
        if temporary.exists() {
            fs::remove_file(&temporary)?;
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Cannot publish Agent runtime status: {error}");
    }
}

fn update_status_from_event(path: &Path, status: &mut RuntimeStatus, event: AgentEvent) {
    match event {
        AgentEvent::Started {
            agent_instance_id,
            relay,
            ..
        } => {
            "started".clone_into(&mut status.status);
            status.agent_instance_id = Some(agent_instance_id);
            status.relay = Some(relay);
        }
        AgentEvent::Connecting => "connecting".clone_into(&mut status.status),
        AgentEvent::Connected {
            pairing_code,
            lease_expires_at,
        } => {
            "waiting".clone_into(&mut status.status);
            status.pairing_code = Some(pairing_code);
            status.pairing_code_expires_at = Some(lease_expires_at);
        }
        AgentEvent::Registered { .. } => {
            "registered_waiting".clone_into(&mut status.status);
            status.pairing_code = None;
            status.pairing_code_expires_at = None;
        }
        AgentEvent::LeaseRenewed { lease_expires_at } => {
            if status.pairing_code.is_some() {
                status.pairing_code_expires_at = Some(lease_expires_at);
            }
        }
        AgentEvent::ControllerCountChanged { active_connections } => {
            status.active_connections = active_connections;
            status.status = if active_connections == 0 {
                if status.pairing_code.is_some() {
                    "waiting".to_owned()
                } else {
                    "registered_waiting".to_owned()
                }
            } else {
                "controlled".to_owned()
            };
        }
        AgentEvent::ControllerBindingsChanged { .. } => {}
        // 操作日志不改变服务状态，避免按输出行重复写入状态文件。
        AgentEvent::OperationLog(_)
        | AgentEvent::OperationStateChanged { .. }
        | AgentEvent::ConnectionEpoch(_)
        | AgentEvent::ScopedOperation { .. }
        | AgentEvent::ControlStatePending(_)
        | AgentEvent::ControlStateResult(_) => return,
        AgentEvent::Reconnecting { .. } => {
            "reconnecting".clone_into(&mut status.status);
            status.active_connections = 0;
            status.pairing_code = None;
            status.pairing_code_expires_at = None;
        }
        AgentEvent::Stopped => "stopped".clone_into(&mut status.status),
        AgentEvent::Failed { .. } => "failed".clone_into(&mut status.status),
    }
    write_status(path, status);
}

#[cfg(windows)]
#[allow(clippy::wildcard_imports)]
mod windows_host {
    use super::*;
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
    };

    define_windows_service!(ffi_service_main, service_main);

    static LAUNCH: std::sync::OnceLock<Launch> = std::sync::OnceLock::new();

    /// 通过 Windows SCM 启动服务入口。
    pub fn start(launch: Launch) -> anyhow::Result<()> {
        let service_name = launch.options.service_name.clone();
        LAUNCH
            .set(launch)
            .map_err(|_| anyhow::anyhow!("服务宿主不能重复启动"))?;
        windows_service::service_dispatcher::start(service_name, ffi_service_main)
            .context("无法注册 RemoteOps Agent Windows Service")?;
        Ok(())
    }

    fn service_main(_arguments: Vec<OsString>) {
        let Some(launch) = LAUNCH.get() else {
            return;
        };
        if let Err(error) = run(launch) {
            eprintln!("RemoteOps Agent Windows Service 运行失败：{error:#}");
        }
    }

    fn run(launch: &Launch) -> anyhow::Result<()> {
        let args = &launch.args;
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let handler_shutdown = shutdown_sender.clone();
        let status_handle =
            service_control_handler::register(&launch.options.service_name, move |control| {
                match control {
                    ServiceControl::Stop | ServiceControl::Shutdown => {
                        let _ = handler_shutdown.send(true);
                        ServiceControlHandlerResult::NoError
                    }
                    ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                    _ => ServiceControlHandlerResult::NotImplemented,
                }
            })?;
        status_handle.set_service_status(start_pending_status(1))?;

        let status_file = args.status_file.clone();
        let (event_sender, event_receiver) = mpsc::channel();
        let status_thread = std::thread::spawn(move || {
            let mut status = RuntimeStatus {
                status: "starting".to_owned(),
                ..RuntimeStatus::default()
            };
            write_status(&status_file, &status);
            while let Ok(event) = event_receiver.recv() {
                update_status_from_event(&status_file, &mut status, event);
            }
        });

        let config = match load_config(&args.config, &launch.profile) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("RemoteOps Agent 配置加载失败：{error:#}");
                drop(event_sender);
                let _ = status_thread.join();
                let _ = status_handle.set_service_status(stopped_status(false));
                let _ = fs::remove_file(&args.status_file);
                return Err(error);
            }
        };
        status_handle.set_service_status(start_pending_status(2))?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .context("无法创建 Agent Service 异步运行时");
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                drop(event_sender);
                let _ = status_thread.join();
                let _ = status_handle.set_service_status(stopped_status(false));
                let _ = fs::remove_file(&args.status_file);
                return Err(error);
            }
        };
        status_handle.set_service_status(running_status())?;

        let agent_shutdown_receiver = shutdown_receiver.clone();
        let mut stop_receiver = shutdown_receiver;
        let permission = AgentPermissionControl::from_config(&config);
        let agent = run_agent_with_options(
            config,
            Some(event_sender),
            agent_shutdown_receiver,
            permission,
            runtime_options(launch, AgentRunMode::WindowsService),
        );
        let status_for_stop = status_handle;
        let result = runtime.block_on(async move {
            tokio::pin!(agent);
            tokio::select! {
                result = &mut agent => result,
                changed = stop_receiver.changed() => {
                    if changed.is_ok() && *stop_receiver.borrow() {
                        let _ = status_for_stop.set_service_status(stop_pending_status(1));
                    }
                    agent.await
                }
            }
        });
        let _ = shutdown_sender.send(true);
        let _ = status_handle.set_service_status(stopped_status(result.is_ok()));
        let _ = status_thread.join();
        let _ = fs::remove_file(&args.status_file);
        result
    }

    fn start_pending_status(checkpoint: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        }
    }

    fn stop_pending_status(checkpoint: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::StopPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint,
            wait_hint: Duration::from_secs(30),
            process_id: None,
        }
    }

    fn running_status() -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        }
    }

    fn stopped_status(success: bool) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: if success {
                ServiceExitCode::Win32(0)
            } else {
                ServiceExitCode::Win32(1)
            },
            checkpoint: 0,
            wait_hint: Duration::from_secs(1),
            process_id: None,
        }
    }
}

async fn run_foreground(launch: &Launch) -> anyhow::Result<()> {
    let args = &launch.args;
    let config = load_config(&args.config, &launch.profile)?;
    let (event_sender, event_receiver) = mpsc::channel();
    let status_file = args.status_file.clone();
    let status_thread = std::thread::spawn(move || {
        let mut status = RuntimeStatus {
            status: "starting".to_owned(),
            ..RuntimeStatus::default()
        };
        write_status(&status_file, &status);
        while let Ok(event) = event_receiver.recv() {
            let previous = status.status.clone();
            update_status_from_event(&status_file, &mut status, event);
            if previous != status.status {
                eprintln!("Agent state: {}", status.status);
            }
        }
        let _ = fs::remove_file(status_file);
    });
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let permission = AgentPermissionControl::from_config(&config);
    let mut task = tokio::spawn(run_agent_with_options(
        config,
        Some(event_sender),
        shutdown_receiver,
        permission,
        runtime_options(launch, AgentRunMode::Console),
    ));
    let result = tokio::select! {
        result = &mut task => result.context("Agent Service task failed")?,
        signal = shutdown_signal() => {
            signal.context("Cannot listen for shutdown signals")?;
            let _ = shutdown_sender.send(true);
            if let Ok(result) = tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                result.context("Agent Service shutdown task failed")?
            } else {
                task.abort();
                let _ = task.await;
                Err(anyhow::anyhow!("Agent Service shutdown timed out"))
            }
        }
    };
    let _ = status_thread.join();
    result
}

/// 运行标准或定制服务产品；共享 SCM、控制台验证、状态和退出管理。
/// # Errors
/// 产品、参数、配置或服务生命周期失败时返回错误。
pub fn run(profile: ProductProfile, options: ServiceOptions) -> anyhow::Result<()> {
    profile.validate()?;
    if options.service_name.is_empty()
        || options.service_name.len() > 128
        || options
            .service_name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        anyhow::bail!("服务名无效");
    }
    let args = Args::parse();
    let product_directory = if profile.legacy_remoteops_paths {
        PathBuf::from(DEFAULT_CONFIG)
            .parent()
            .context("内置服务配置路径缺少父目录")?
            .to_path_buf()
    } else {
        #[cfg(windows)]
        let root = std::env::var_os("ProgramData")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
        #[cfg(not(windows))]
        let root = PathBuf::from("/var/lib");
        root.join(&profile.directory_name).join("Agent")
    };
    let default_status = if profile.legacy_remoteops_paths {
        PathBuf::from(DEFAULT_STATUS)
    } else {
        product_directory.join("runtime-status.json")
    };
    let launch = Launch {
        args: ResolvedArgs {
            config: args
                .config
                .or(options.config_file.clone())
                .unwrap_or_else(|| product_directory.join(&profile.config_file_name)),
            status_file: args
                .status_file
                .or(options.status_file.clone())
                .unwrap_or(default_status),
            console: args.console,
            check_config: args.check_config,
        },
        profile,
        options,
    };
    remoteops_agent::initialize_tracing();
    if launch.args.check_config {
        let config = load_config(&launch.args.config, &launch.profile)?;
        if let Some(ca) = config.ca_cert.as_deref() {
            fs::File::open(ca).context("Cannot read configured CA certificate")?;
        }
        fs::create_dir_all(&config.transfer_root)?;
        fs::create_dir_all(
            config
                .state_file
                .parent()
                .context("State path has no parent")?,
        )?;
        remoteops_agent::validate_managed_enrollment(&config, launch.options.runtime.pairing_mode)?;
        println!("Agent configuration validated");
        return Ok(());
    }
    if launch.args.console || cfg!(not(windows)) {
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(run_foreground(&launch));
    }
    #[cfg(windows)]
    {
        windows_host::start(launch)
    }
    #[cfg(not(windows))]
    {
        unreachable!()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn service_status_is_private_and_clears_expired_pairing() {
        let dir = std::env::temp_dir().join(format!("remoteops-status-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("status.json");
        let mut status = RuntimeStatus::default();
        update_status_from_event(
            &path,
            &mut status,
            AgentEvent::Connected {
                pairing_code: "123-456-789".into(),
                lease_expires_at: Utc::now(),
            },
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        update_status_from_event(
            &path,
            &mut status,
            AgentEvent::Reconnecting {
                message: "offline".into(),
                retry_seconds: 1,
            },
        );
        let data: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(data["pairing_code"].is_null());
        assert_eq!(data["status"], "reconnecting");
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod operation_log_tests {
    use super::*;
    use remoteops_agent::{AgentLogLevel, AgentOperationLog};

    /// 日志事件不产生服务状态文件，也不改变当前运行状态。
    #[test]
    fn operation_log_does_not_publish_runtime_status() {
        let path = std::env::temp_dir().join(format!(
            "remoteops-log-status-{}.json",
            remoteops_domain::RequestId::new()
        ));
        let mut status = RuntimeStatus {
            status: "controlled".to_owned(),
            ..RuntimeStatus::default()
        };
        update_status_from_event(
            &path,
            &mut status,
            AgentEvent::OperationLog(AgentOperationLog {
                request_id: None,
                occurred_at: Utc::now(),
                level: AgentLogLevel::Info,
                message: "example command output".to_owned(),
            }),
        );
        assert_eq!(status.status, "controlled");
        assert!(!path.exists());
        for state in [
            remoteops_agent::AgentOperationState::Running,
            remoteops_agent::AgentOperationState::Succeeded,
            remoteops_agent::AgentOperationState::Failed,
        ] {
            update_status_from_event(
                &path,
                &mut status,
                AgentEvent::OperationStateChanged {
                    request_id: remoteops_domain::RequestId::new(),
                    state,
                },
            );
            assert_eq!(status.status, "controlled");
            assert!(!path.exists());
        }
    }
}

#[cfg(test)]
mod product_tests {
    use super::*;
    #[test]
    fn service_defaults_use_machine_config_directory() {
        let directory =
            std::env::temp_dir().join(format!("remoteops-service-config-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let config_file = directory.join("agent-config.json");
        fs::write(&config_file, b"{}").unwrap();
        let profile = ProductProfile {
            id: "service-path-test".into(),
            endpoint: remoteops_agent::EndpointPolicy::Fixed {
                relay: "relay.example.invalid:7443".into(),
                server_name: "relay.example.invalid".into(),
            },
            legacy_remoteops_paths: false,
            ..ProductProfile::default()
        };
        let config = load_config(&config_file, &profile).unwrap();
        assert_eq!(config.state_file.parent(), Some(directory.as_path()));
        assert_eq!(config.transfer_root, directory.join("transfer"));
        fs::remove_file(config_file).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn custom_service_and_console_share_the_product_ceiling() {
        let launch = Launch {
            profile: ProductProfile {
                id: "ops-center".into(),
                legacy_remoteops_paths: false,
                ..ProductProfile::default()
            },
            options: ServiceOptions::new("OpsCenterAgent").managed().read_only(),
            args: ResolvedArgs {
                config: "config.json".into(),
                status_file: "status.json".into(),
                console: false,
                check_config: false,
            },
        };
        for mode in [AgentRunMode::WindowsService, AgentRunMode::Console] {
            let options = runtime_options(&launch, mode);
            assert_eq!(options.product.id, "ops-center");
            assert_eq!(options.run_mode, mode);
            assert!(!options.visual_enabled);
            assert!(!options.capability_policy.visual_allowed());
            assert!(options.capability_policy.read_only());
            assert_eq!(
                options.pairing_mode,
                remoteops_protocol::PairingMode::ManagedOnDemand
            );
        }
    }
}
