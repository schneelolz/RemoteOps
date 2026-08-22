#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use clap::Parser;
use remoteops_agent::{AgentConfig, AgentEvent, run_agent};
use remoteops_domain::AgentInstanceId;
use serde::Serialize;
use tokio::sync::watch;

const SERVICE_NAME: &str = "RemoteOpsAgent";

/// `RemoteOps` Agent 服务宿主参数。
#[derive(Debug, Parser)]
#[command(version, about = "RemoteOps 可选 Windows Agent 服务")]
struct Args {
    /// Agent 配置文件；服务安装脚本会写入机器级目录。
    #[arg(
        long,
        default_value = r"C:\ProgramData\RemoteOps\Agent\agent-config.json"
    )]
    config: PathBuf,
    /// 服务运行状态文件；停止服务后自动删除。
    #[arg(
        long,
        default_value = r"C:\ProgramData\RemoteOps\Agent\runtime-status.json"
    )]
    status_file: PathBuf,
    /// 在前台运行，用于安装前验证配置，不注册 SCM 服务。
    #[arg(long)]
    console: bool,
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

fn load_config(path: &Path) -> anyhow::Result<AgentConfig> {
    let config = AgentConfig::load_file(Some(path))?;
    config.normalize_and_validate()
}

fn write_status(path: &Path, status: &RuntimeStatus) {
    let Ok(contents) = serde_json::to_vec_pretty(status) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension("json.tmp");
    if fs::write(&temporary, contents).is_ok() {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        let _ = fs::rename(temporary, path);
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
        AgentEvent::ControllerCountChanged { active_connections } => {
            status.active_connections = active_connections;
            status.status = if active_connections == 0 {
                "waiting".to_owned()
            } else {
                "controlled".to_owned()
            };
        }
        AgentEvent::ControllerBindingsChanged { .. } => {}
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

    /// 通过 Windows SCM 启动服务入口。
    pub fn start() -> anyhow::Result<()> {
        windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .context("无法注册 RemoteOps Agent Windows Service")?;
        Ok(())
    }

    fn service_main(_arguments: Vec<OsString>) {
        let args = Args::parse();
        if let Err(error) = run(args) {
            eprintln!("RemoteOps Agent Windows Service 运行失败：{error:#}");
        }
    }

    fn run(args: Args) -> anyhow::Result<()> {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let handler_shutdown = shutdown_sender.clone();
        let status_handle =
            service_control_handler::register(SERVICE_NAME, move |control| match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = handler_shutdown.send(true);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
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

        let config = match load_config(&args.config) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("RemoteOps Agent 配置加载失败：{error:#}");
                drop(event_sender);
                let _ = status_thread.join();
                let _ = status_handle.set_service_status(stopped_status(false));
                let _ = fs::remove_file(args.status_file);
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
                let _ = fs::remove_file(args.status_file);
                return Err(error);
            }
        };
        status_handle.set_service_status(running_status())?;

        let agent_shutdown_receiver = shutdown_receiver.clone();
        let mut stop_receiver = shutdown_receiver;
        let agent = run_agent(config, Some(event_sender), agent_shutdown_receiver);
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
        let _ = fs::remove_file(args.status_file);
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

#[cfg(not(windows))]
mod windows_host {
    pub fn start() -> anyhow::Result<()> {
        anyhow::bail!("remoteops-agent-service 仅支持 Windows")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.console {
        let config = load_config(&args.config)?;
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let mut task = tokio::spawn(run_agent(config, None, shutdown_receiver));
        return tokio::select! {
            result = &mut task => result.context("Agent Service 前台任务异常结束")?,
            signal = tokio::signal::ctrl_c() => {
                signal.context("无法监听 Ctrl+C")?;
                let _ = shutdown_sender.send(true);
                if let Ok(result) = tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                    result.context("Agent Service 前台停止任务异常结束")?
                } else {
                    task.abort();
                    let _ = task.await;
                    Ok(())
                }
            }
        };
    }
    windows_host::start()
}

