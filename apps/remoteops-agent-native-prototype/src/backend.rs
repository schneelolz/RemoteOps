//! 与原生窗口隔离的后台任务；通过有界事件队列报告，不直接访问 HWND。

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::model::{Event, safe_record, sample_line};

/// 小样可运行的明确场景；演示不表示真实 SSH 或串口连接。
#[derive(Clone, Copy, Debug)]
pub enum Scenario {
    /// 使用现有设备适配器执行固定的本地只读查询。
    LocalQuery,
    /// 展示明确标注的 SSH 交互样例。
    SshDemo,
    /// 展示明确标注的 Console 交互样例。
    ConsoleDemo,
    /// 生成指定行数的有界日志压力流。
    Stress(u64),
}

struct Worker {
    // 线程只使用自己的停止信号，不借用窗口状态或生命周期。
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct CompletionGuard(Arc<AtomicBool>);

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
struct Emitter {
    sender: SyncSender<Event>,
    produced: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    // 与任务及窗口关闭共用停止信号，背压等待随取消退出。
    cancel: Arc<AtomicBool>,
    // 脱敏状态先于入队更新，起始行被丢弃也不能泄漏后续 PEM 正文。
    private_key: Arc<Mutex<bool>>,
}

impl Emitter {
    fn line(&self, text: &str) {
        self.produced.fetch_add(1, Ordering::Relaxed);
        let mut event = Event::Line(self.sanitize(text));
        loop {
            if self.cancel.load(Ordering::Acquire) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            match self.sender.try_send(event) {
                Ok(()) => return,
                Err(TrySendError::Full(pending)) => {
                    // 不持有窗口或脱敏锁；只暂停生产者，界面仍可持续排空队列。
                    event = pending;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    self.cancel.store(true, Ordering::Release);
                    return;
                }
            }
        }
    }

    fn status(&self, text: &str) {
        let _ = self.sender.try_send(Event::Status(self.sanitize(text)));
    }

    fn sanitize(&self, text: &str) -> String {
        let upper = text.to_ascii_uppercase();
        let begins_key = upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY-----");
        let ends_key = upper.contains("-----END ") && upper.contains("PRIVATE KEY-----");
        let mut private_key = self
            .private_key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *private_key || begins_key {
            *private_key = !ends_key;
            "[私钥内容已隐藏]".to_owned()
        } else {
            safe_record(text)
        }
    }

    fn finished(&self) {
        let _ = self.sender.try_send(Event::Finished);
    }
}

/// 单任务后台宿主；任务结束不依赖界面是否及时读取完成事件。
pub struct Backend {
    sender: SyncSender<Event>,
    worker: Option<Worker>,
    produced: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl Backend {
    /// 连接由界面拥有的有界事件队列，不启动任务或读取配置。
    #[must_use]
    pub fn new(sender: SyncSender<Event>) -> Self {
        Self {
            sender,
            worker: None,
            produced: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 启动一个明确场景；前一任务未结束时拒绝重入。
    ///
    /// # Errors
    /// 前一任务未结束或系统无法创建线程时返回错误。
    pub fn start(&mut self, scenario: Scenario) -> Result<(), String> {
        self.launch(move |emitter, cancel| match scenario {
            Scenario::LocalQuery => local_query(&emitter, &cancel),
            Scenario::SshDemo => demonstration(&emitter, &cancel, true),
            Scenario::ConsoleDemo => demonstration(&emitter, &cancel, false),
            Scenario::Stress(lines) => {
                emitter.status("正在生成压力样例；队列拥塞时可取消地等待界面，不丢弃日志");
                for index in 0..lines {
                    if cancel.load(Ordering::Acquire) {
                        break;
                    }
                    emitter.line(&sample_line(index));
                    if index.is_multiple_of(256) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                emitter.status("压力样例已结束；生成、丢弃和保留数量分别统计");
            }
        })
    }

    /// 返回真实后台线程当前是否仍在运行。
    #[must_use]
    pub fn busy(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| worker.running.load(Ordering::Acquire))
    }

    /// 请求停止本次任务；调用本身不会等待后台或事件队列。
    pub fn stop(&self) {
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Release);
        }
    }

    /// 返回本次任务尝试产生的日志条数。
    #[must_use]
    pub fn produced_lines(&self) -> u64 {
        self.produced.load(Ordering::Relaxed)
    }

    /// 返回本次任务因取消或接收端退出而未送达的日志条数。
    #[must_use]
    pub fn dropped_lines(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 请求退出并最多等待两秒；返回 false 表示线程超时或异常结束。
    pub fn shutdown(&mut self) -> bool {
        self.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.handle.is_finished())
        {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
        self.worker
            .take()
            .is_none_or(|worker| worker.handle.join().is_ok())
    }

    fn launch(
        &mut self,
        operation: impl FnOnce(Emitter, Arc<AtomicBool>) + Send + 'static,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("已有任务正在运行，请先停止当前任务".to_owned());
        }
        if let Some(worker) = self.worker.take() {
            worker
                .handle
                .join()
                .map_err(|_| "上一任务异常结束".to_owned())?;
        }
        self.produced.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let emitter = Emitter {
            sender: self.sender.clone(),
            produced: self.produced.clone(),
            dropped: self.dropped.clone(),
            cancel: cancel.clone(),
            private_key: Arc::new(Mutex::new(false)),
        };
        let child_cancel = cancel.clone();
        let child_running = running.clone();
        let handle = thread::Builder::new()
            .name("remoteops-native-prototype-worker".to_owned())
            .spawn(move || {
                let _completion = CompletionGuard(child_running);
                operation(emitter.clone(), child_cancel);
                emitter.finished();
            })
            .map_err(|error| format!("无法创建后台线程：{error}"))?;
        self.worker = Some(Worker {
            cancel,
            running,
            handle,
        });
        Ok(())
    }

    /// 仅根据调用方明确指定的配置启动真实 Agent，不发现或读取日常配置。
    ///
    /// # Errors
    /// 配置不存在、配置无效、前一任务未结束或线程创建失败时返回错误。
    #[cfg(feature = "agent-runtime")]
    pub fn start_agent(&mut self, config_path: &std::path::Path) -> Result<(), String> {
        let profile = remoteops_product::ProductProfile {
            id: "remoteops-agent-native-prototype".to_owned(),
            name: "RemoteOps 原生界面小样".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            directory_name: "RemoteOpsNativePrototype".to_owned(),
            config_file_name: "native-prototype-config.json".to_owned(),
            state_file_name: "native-prototype-state.json".to_owned(),
            env_prefix: "REMOTEOPS_NATIVE_PROTOTYPE".to_owned(),
            legacy_remoteops_paths: false,
            ..remoteops_product::ProductProfile::default()
        };
        let config =
            remoteops_agent::AgentConfig::load_file_with_profile(Some(config_path), &profile)
                .and_then(remoteops_agent::AgentConfig::normalize_and_validate)
                .map_err(|error| safe_record(&format!("显式 Agent 配置无效：{error}")))?;
        self.launch(move |emitter, cancel| agent_runtime(&emitter, &cancel, config, profile))
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // 析构不能因为队列已满或后台异常而阻塞窗口线程。
        self.stop();
    }
}

fn demonstration(emitter: &Emitter, cancel: &AtomicBool, ssh: bool) {
    let (label, records): (&str, &[&str]) = if ssh {
        (
            "SSH 交互演示（未连接交换机）",
            &[
                "[演示] 正在建立 SSH 连接；这不是实际网络操作",
                "[演示] 身份认证信息已隐藏",
                "[演示·发送] display version",
                "[演示·返回] 设备版本信息 / 中文输出 🔎",
                "[演示] SSH 场景展示结束",
            ],
        )
    } else {
        (
            "Console 交互演示（未打开串口）",
            &[
                "[演示] Console 9600 8N1；这不是实际串口操作",
                "[演示·返回] <HUAWEI>",
                "[演示·发送] display interface brief",
                "[演示·返回] 接口状态：正常；链路状态：已连接",
                "[演示] Console 场景展示结束",
            ],
        )
    };
    emitter.status(label);
    for record in records {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        emitter.line(record);
        for _ in 0..10 {
            if cancel.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    emitter.status("演示已结束");
}

#[cfg(not(feature = "agent-runtime"))]
fn local_query(emitter: &Emitter, _cancel: &AtomicBool) {
    emitter.status("仅界面构建；本地查询需要带 Agent 核心的版本");
    emitter.line("[未执行] 该构建只用于测量 Windows 原生界面体积");
}

#[cfg(feature = "agent-runtime")]
fn local_query(emitter: &Emitter, cancel: &AtomicBool) {
    use remoteops_device::{ShellProvider, SystemDevice};
    use remoteops_domain::ShellKind;

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            emitter.status(&format!("无法创建查询运行时：{error}"));
            return;
        }
    };
    runtime.block_on(async {
        let device = SystemDevice::new();
        #[cfg(windows)]
        let (shell, command) = (
            ShellKind::WindowsPowerShell,
            "Write-Output '原生界面本地只读查询：中文正常 🔎'; Get-Date -Format 'yyyy-MM-dd HH:mm:ss'",
        );
        #[cfg(not(windows))]
        let (shell, command) = (ShellKind::System, "printf '本地只读查询：中文正常\\n'; date");
        emitter.status("正在执行固定的本地只读查询");
        emitter.line(&format!("[实际发送] {command}"));
        let (output, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let future = device.run_streaming(shell, command, 15, output);
        tokio::pin!(future);
        let mut stdout = StreamLines::default();
        let mut stderr = StreamLines::default();
        let result = loop {
            tokio::select! {
                result = &mut future => break Some(result),
                Some(chunk) = receiver.recv() => {
                    if chunk.stderr { stderr.push(&chunk.text, emitter); }
                    else { stdout.push(&chunk.text, emitter); }
                }
                () = tokio::time::sleep(Duration::from_millis(20)) => {
                    if cancel.load(Ordering::Acquire) { break None; }
                }
            }
        };
        while let Ok(chunk) = receiver.try_recv() {
            if chunk.stderr { stderr.push(&chunk.text, emitter); }
            else { stdout.push(&chunk.text, emitter); }
        }
        stdout.finish(emitter);
        stderr.finish(emitter);
        match result {
            Some(Ok(result)) => emitter.status(&format!("本地只读查询结束，退出码 {:?}", result.exit_code)),
            Some(Err(error)) => emitter.status(&format!("本地只读查询失败：{error}")),
            None => emitter.status("本地只读查询已取消"),
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(200));
}

#[cfg(feature = "agent-runtime")]
#[derive(Default)]
struct StreamLines {
    pending: String,
    units: usize,
    discarded: bool,
}

#[cfg(feature = "agent-runtime")]
impl StreamLines {
    fn push(&mut self, text: &str, emitter: &Emitter) {
        for character in text.chars() {
            if character == '\n' {
                self.finish(emitter);
            } else if !self.discarded {
                self.units += character.len_utf16();
                if self.units > 16_384 {
                    self.pending.clear();
                    self.discarded = true;
                } else {
                    self.pending.push(character);
                }
            }
        }
    }

    fn finish(&mut self, emitter: &Emitter) {
        if self.discarded {
            emitter.line("[超长输出行已隐藏]");
        } else if !self.pending.is_empty() {
            emitter.line(self.pending.trim_end_matches('\r'));
        }
        self.pending.clear();
        self.units = 0;
        self.discarded = false;
    }
}

#[cfg(feature = "agent-runtime")]
fn agent_runtime(
    emitter: &Emitter,
    cancel: &AtomicBool,
    config: remoteops_agent::AgentConfig,
    profile: remoteops_product::ProductProfile,
) {
    use remoteops_agent::{AgentPermissionControl, AgentRuntimeOptions};

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            emitter.status(&format!("无法创建 Agent 运行时：{error}"));
            return;
        }
    };
    runtime.block_on(async {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let permissions = AgentPermissionControl::from_config(&config);
        let mut agent = tokio::spawn(remoteops_agent::run_agent_with_scoped_options(
            config,
            Some(sender),
            shutdown_receiver,
            permissions,
            AgentRuntimeOptions::interactive(profile),
        ));
        let mut epoch = None;
        emitter.status("已显式启动 Agent 核心；保持现有权限和会话验证");
        loop {
            // 高吞吐事件也必须给取消和 Agent 生命周期检查留下执行机会。
            for _ in 0..256 {
                let Ok(event) = receiver.try_recv() else { break; };
                present_agent_event(event, &mut epoch, emitter);
            }
            tokio::select! {
                result = &mut agent => {
                    match result {
                        Ok(Ok(())) => emitter.status("Agent 核心已退出"),
                        Ok(Err(error)) => emitter.status(&format!("Agent 核心失败：{error}")),
                        Err(_) => emitter.status("Agent 后台任务异常结束"),
                    }
                    break;
                }
                () = tokio::time::sleep(Duration::from_millis(20)) => {
                    if cancel.load(Ordering::Acquire) {
                        let _ = shutdown.send(true);
                        if tokio::time::timeout(Duration::from_millis(1_200), &mut agent).await.is_err() {
                            agent.abort();
                            let _ = agent.await;
                        }
                        emitter.status("Agent 停止请求已处理");
                        break;
                    }
                }
            }
        }
        for event in receiver.try_iter().take(256) {
            present_agent_event(event, &mut epoch, emitter);
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(200));
}

#[cfg(feature = "agent-runtime")]
fn present_agent_event(
    mut event: remoteops_agent::AgentEvent,
    epoch: &mut Option<u64>,
    emitter: &Emitter,
) {
    use remoteops_agent::AgentEvent;

    if let AgentEvent::ConnectionEpoch(value) = event {
        *epoch = Some(value);
        return;
    }
    if let AgentEvent::ScopedOperation {
        epoch: value,
        event: inner,
    } = event
    {
        if *epoch != Some(value) {
            return;
        }
        event = *inner;
    }
    match event {
        AgentEvent::OperationLog(log) => emitter.line(&log.message),
        AgentEvent::OperationStateChanged { state, .. } => {
            emitter.status(&format!("远程操作状态：{state:?}"));
        }
        AgentEvent::Connecting => emitter.status("Agent 正在连接显式指定的 Relay"),
        AgentEvent::Connected { .. } => emitter.status("Agent 已连接，等待工程师绑定"),
        AgentEvent::Registered { .. } => emitter.status("受管 Agent 已登记待命"),
        AgentEvent::ControllerCountChanged { active_connections } => {
            emitter.status(&format!("已绑定控制端：{active_connections}"));
        }
        AgentEvent::Reconnecting { .. } => emitter.status("Agent 已断开，等待重连"),
        AgentEvent::Failed { message } => emitter.status(&format!("Agent 失败：{message}")),
        AgentEvent::Stopped => emitter.status("Agent 已停止"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_queue_does_not_block_cancellation_or_completion() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let mut backend = Backend::new(sender);
        backend.start(Scenario::Stress(100_000)).unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while backend.produced_lines() == 0 && Instant::now() < ready_deadline {
            thread::sleep(Duration::from_millis(2));
        }
        let started = Instant::now();
        assert!(backend.shutdown());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!backend.busy());
        assert!(backend.dropped_lines() > 0);
        assert!(backend.produced_lines() >= backend.dropped_lines());
    }

    #[test]
    fn reentry_is_rejected_and_counts_reset_for_next_task() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let mut backend = Backend::new(sender);
        backend.start(Scenario::SshDemo).unwrap();
        assert!(backend.start(Scenario::ConsoleDemo).is_err());
        assert!(backend.shutdown());
        backend.start(Scenario::Stress(0)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while backend.busy() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!backend.busy());
        assert_eq!(backend.produced_lines(), 0);
        assert!(backend.shutdown());
    }

    #[test]
    fn demonstration_is_explicitly_labelled_and_never_echoes_a_password() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(64);
        let mut backend = Backend::new(sender);
        backend.start(Scenario::SshDemo).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut observed_demo_line = false;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match receiver.recv_timeout(remaining) {
                Ok(Event::Line(text))
                    if text.contains("[演示]") && text.contains("这不是实际网络操作") =>
                {
                    observed_demo_line = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(backend.shutdown());
        assert!(
            observed_demo_line,
            "未在期限内收到明确标注为演示的实际日志行"
        );
    }

    #[test]
    fn complete_log_stream_survives_bounded_queue_backpressure_without_loss() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        let mut backend = Backend::new(sender);
        backend.start(Scenario::Stress(10_000)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut delivered = 0_u64;
        loop {
            if let Ok(Event::Line(_)) = receiver.recv_timeout(Duration::from_millis(10)) {
                delivered += 1;
            }
            if !backend.busy() {
                delivered += u64::try_from(
                    receiver
                        .try_iter()
                        .filter(|event| matches!(event, Event::Line(_)))
                        .count(),
                )
                .unwrap();
                break;
            }
            assert!(Instant::now() < deadline, "完整日志传送超时");
        }
        assert_eq!(delivered, 10_000);
        assert_eq!(backend.produced_lines(), 10_000);
        assert_eq!(backend.dropped_lines(), 0);
        assert!(backend.shutdown());
    }

    #[test]
    fn private_key_body_stays_masked_while_queue_backpressure_is_active() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let emitter = Emitter {
            sender,
            produced: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            private_key: Arc::new(Mutex::new(false)),
        };
        emitter.status("先占满队列");
        let worker_emitter = emitter.clone();
        let worker = thread::spawn(move || {
            worker_emitter.line("-----BEGIN PRIVATE KEY-----");
            worker_emitter.line("private-material-must-stay-hidden");
            worker_emitter.line("-----END PRIVATE KEY-----");
            worker_emitter.line("恢复中文输出");
        });
        let _ = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..3 {
            let event = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(event, Event::Line(text) if text == "[私钥内容已隐藏]"));
        }
        assert!(
            matches!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), Event::Line(text) if text == "恢复中文输出")
        );
        worker.join().unwrap();
        assert_eq!(emitter.produced.load(Ordering::Relaxed), 4);
        assert_eq!(emitter.dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn disconnected_receiver_cancels_producer_without_hanging() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        drop(receiver);
        let mut backend = Backend::new(sender);
        backend.start(Scenario::Stress(100_000)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while backend.busy() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(!backend.busy());
        assert_eq!(backend.produced_lines(), 1);
        assert_eq!(backend.dropped_lines(), 1);
        assert!(backend.shutdown());
    }

    #[cfg(feature = "agent-runtime")]
    #[test]
    fn split_output_is_only_published_after_a_complete_line_and_redaction() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        let emitter = Emitter {
            sender,
            produced: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            private_key: Arc::new(Mutex::new(false)),
        };
        let mut lines = StreamLines::default();
        lines.push("pass", &emitter);
        lines.push("word=原型", &emitter);
        assert!(receiver.try_recv().is_err());
        lines.push("测试秘密\n中文输出🔎\n", &emitter);
        let output: Vec<_> = receiver.try_iter().collect();
        assert_eq!(output.len(), 2);
        assert!(
            matches!(&output[0], Event::Line(text) if !text.contains("原型") && !text.contains("测试秘密"))
        );
        assert!(matches!(&output[1], Event::Line(text) if text.contains("中文输出🔎")));
    }
}
