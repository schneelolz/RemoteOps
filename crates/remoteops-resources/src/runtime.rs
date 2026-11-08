use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex, MutexGuard},
    time::Duration,
};

use process_wrap::tokio::ChildWrapper;
#[cfg(unix)]
use process_wrap::tokio::{CommandWrap, KillOnDrop, ProcessGroup};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    process::{ChildStdin, ChildStdout, Command},
    sync::{Mutex, Notify, watch},
    time::timeout,
};
use uuid::Uuid;

use crate::{
    BrowserAdapter, ExecutionInterface, ResourceConfig, ResourceError, ResourceTool, Result, cache,
    diagnostics,
};

#[cfg(test)]
#[path = "tests.rs"]
mod integration_tests;

#[path = "browser_password.rs"]
mod browser_password;

const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_INSTANCES: usize = 8;

/// 外部工具的调用 Schema，权限始终由 `RemoteOps` 上层决定。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallToolSchema {
    /// 上游工具名或固定 CLI 适配工具名。
    pub name: String,
    /// 供 AI 了解用途的文本。
    pub description: String,
    /// JSON Schema 参数；来自上游的描述不能授予权限。
    pub input_schema: Value,
}

/// 仅与当前 `RemoteOps` 会话绑定的资源工具实例。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartedTool {
    /// 不透明实例标识。
    pub instance_id: String,
    /// 精确工具标识。
    pub tool_id: String,
    /// 固定版本。
    pub version: String,
    /// 固定包摘要。
    pub sha256: String,
    /// 已通过健康检查的调用 Schema。
    pub tools: Vec<CallToolSchema>,
}

/// 已认证中继通道的包下载回调；返回绝对 ZIP 临时文件路径。
pub type PackageFetcher = Arc<
    dyn Fn(
            crate::ResourcePackage,
            Option<crate::ProgressReporter>,
        ) -> Pin<Box<dyn Future<Output = Result<PathBuf>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Default, PartialEq, Eq)]
struct Catalog {
    revision: u64,
    enabled: bool,
    guide_markdown: String,
    tools: Vec<ResourceTool>,
}

/// Agent、CLI 和 MCP 复用的资源管理器；进程和临时状态不跨会话复用。
pub struct ResourceManager {
    config: ResourceConfig,
    root: PathBuf,
    catalog: StdMutex<Option<Catalog>>,
    package_fetcher: StdMutex<Option<PackageFetcher>>,
    client: reqwest::Client,
    // 只有缓存准备串行；运行时不持有此锁，避免长操作阻塞停止。
    preparation: Mutex<()>,
    instances: StdMutex<HashMap<String, Arc<Handle>>>,
    starting: StdMutex<HashMap<String, PendingStart>>,
    starts_done: Notify,
    cleanup: Arc<crate::cleanup::CleanupWorkers>,
}

struct DownloadArchive(PathBuf);

impl Drop for DownloadArchive {
    fn drop(&mut self) {
        // Future 被取消后仍回收传输临时包，缓存提交持有自己的副本。
        let _ = std::fs::remove_file(&self.0);
    }
}

struct PendingStart {
    session: String,
    identity: (String, String, String),
    cancel: watch::Sender<bool>,
}

struct StartRegistration<'a> {
    key: String,
    manager: &'a ResourceManager,
}

impl Drop for StartRegistration<'_> {
    fn drop(&mut self) {
        self.manager
            .starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
        self.manager.starts_done.notify_waiters();
    }
}

struct ActiveCall<'a> {
    key: &'a str,
    manager: &'a ResourceManager,
    handle: &'a Arc<Handle>,
    armed: bool,
}

impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut instances = self.manager.instances();
            if instances
                .get(self.key)
                .is_some_and(|handle| Arc::ptr_eq(handle, self.handle))
            {
                instances.remove(self.key);
            }
        }
    }
}

struct Handle {
    session: String,
    identity: (String, String, String),
    cancel: watch::Sender<bool>,
    instance: Mutex<Option<Instance>>,
}

impl Handle {
    fn cancel_now(&self) {
        self.cancel.send_replace(true);
        // 空闲实例没有等待取消信号的调用任务；先终止其进程树，目录继续后台清理。
        if let Ok(mut state) = self.instance.try_lock() {
            drop(state.take());
        }
    }
}

struct Instance {
    tool: ResourceTool,
    package: PathBuf,
    process: Option<ManagedProcess>,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    tools: Vec<CallToolSchema>,
    sequence: u64,
    password_binding: Option<browser_password::PasswordBinding>,
    password_redactions: Vec<zeroize::Zeroizing<String>>,
    run: Option<tempfile::TempDir>,
    startup_trace: Arc<StdMutex<diagnostics::Trace>>,
    cleanup: Arc<crate::cleanup::CleanupWorkers>,
    #[cfg(windows)]
    job: Option<Arc<win32job::Job>>,
}

struct ManagedProcess {
    child: Box<dyn ChildWrapper>,
    #[cfg(windows)]
    job: Option<Arc<win32job::Job>>,
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = self.child.inner_mut().start_kill();
        #[cfg(not(windows))]
        let _ = self.child.start_kill();
        #[cfg(windows)]
        drop(self.job.take());
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Job/进程组必须先终止，随后才回收其占用的 profile。
        drop(self.process.take());
        #[cfg(windows)]
        drop(self.job.take());
        if let Some(run) = self.run.take() {
            self.cleanup.schedule(run.keep());
        }
    }
}

impl ResourceManager {
    fn instances(&self) -> MutexGuard<'_, HashMap<String, Arc<Handle>>> {
        self.instances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cancel_starts(&self, session: Option<&str>) {
        let starts = self
            .starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for pending in starts
            .values()
            .filter(|pending| session.is_none_or(|session| pending.session == session))
        {
            pending.cancel.send_replace(true);
        }
    }

    async fn wait_starts(&self, session: Option<&str>) -> Result<()> {
        timeout(Duration::from_secs(5), async {
            loop {
                let finished = self.starts_done.notified();
                tokio::pin!(finished);
                finished.as_mut().enable();
                if !self
                    .starting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .values()
                    .any(|pending| session.is_none_or(|session| pending.session == session))
                {
                    break;
                }
                finished.await;
            }
        })
        .await
        .map_err(|_| crate::cleanup::pending())
    }

    /// 创建资源管理器，不下载或启动任何程序。
    ///
    /// # Errors
    /// 配置无效或 HTTPS 客户端初始化失败时拒绝。
    pub fn new(config: ResourceConfig, cache_dir: PathBuf) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            root: cache_dir,
            catalog: StdMutex::new(None),
            package_fetcher: StdMutex::new(None),
            client: cache::client()?,
            preparation: Mutex::new(()),
            instances: StdMutex::new(HashMap::new()),
            starting: StdMutex::new(HashMap::new()),
            starts_done: Notify::new(),
            cleanup: Arc::default(),
        })
    }

    /// 创建只接受已认证中继目录的管理器，初始策略关闭。
    ///
    /// # Errors
    /// HTTPS 客户端无法初始化时返回错误。
    pub fn new_centralized(cache_dir: PathBuf) -> Result<Self> {
        let manager = Self::new(ResourceConfig::default(), cache_dir)?;
        *manager
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Catalog::default());
        Ok(manager)
    }

    /// 连接或重连时替换已认证包下载通道。
    pub fn set_package_fetcher(&self, fetcher: PackageFetcher) {
        *self
            .package_fetcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fetcher);
    }

    /// 返回当前集中目录修订号；本地模式返回空值。
    #[must_use]
    pub fn catalog_revision(&self) -> Option<u64> {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|value| value.revision)
    }

    /// 同步发布中继目录并取消被撤销工具；接收端逐条调用，避免快速停用被合并。
    ///
    /// # Errors
    /// 拒绝旧修订或无效清单。
    pub fn publish_catalog(
        &self,
        revision: u64,
        enabled: bool,
        tools: Vec<ResourceTool>,
    ) -> Result<()> {
        self.publish_catalog_with_guide(revision, enabled, tools, String::new())
    }

    /// 同步发布目录和全局说明；说明变化不撤销仍获准的实例。
    ///
    /// # Errors
    /// 拒绝旧修订、无效清单或超过 32 KiB 的说明。
    pub fn publish_catalog_with_guide(
        &self,
        revision: u64,
        enabled: bool,
        tools: Vec<ResourceTool>,
        guide_markdown: String,
    ) -> Result<()> {
        if guide_markdown.len() > 32 * 1024 || guide_markdown.contains('\0') {
            return Err(ResourceError::before(
                "invalid_guide",
                "全局说明超过 32 KiB 或含 NUL",
            ));
        }
        let bytes = serde_json::to_vec(&crate::ResourceIndex {
            schema_version: crate::RESOURCE_TOOLS_SCHEMA_VERSION,
            tools: tools.clone(),
        })
        .map_err(|_| ResourceError::before("invalid_manifest", "目录序列化失败"))?;
        crate::validate_manifest(&bytes)?;
        {
            let mut catalog = self
                .catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if catalog
                .as_ref()
                .is_some_and(|current| current.revision > revision)
            {
                return Err(ResourceError::before("stale_catalog", "拒绝过期中继目录"));
            }
            let next = Catalog {
                revision,
                enabled,
                guide_markdown,
                tools,
            };
            if catalog
                .as_ref()
                .is_some_and(|current| current.revision == revision && current != &next)
            {
                return Err(ResourceError::before(
                    "catalog_conflict",
                    "相同修订不能覆盖不同的中继目录",
                ));
            }
            *catalog = Some(next);
        }
        {
            let starts = self
                .starting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for pending in starts.values() {
                if !self.identity_allowed(&pending.identity) {
                    pending.cancel.send_replace(true);
                }
            }
        }
        for handle in self.instances().values() {
            if !self.identity_allowed(&handle.identity) {
                handle.cancel_now();
            }
        }
        Ok(())
    }

    /// 新认证连接开始前清空策略修订；旧连接目录不能影响新连接。
    pub fn reset_catalog(&self) {
        *self
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Catalog::default());
        self.cancel_starts(None);
        for handle in self.instances().values() {
            handle.cancel_now();
        }
    }

    /// 应用目录并等待被取消实例的清理；门禁应在连接读取任务先同步发布。
    ///
    /// # Errors
    /// 拒绝旧修订、无效清单或返回未完成的进程清理。
    pub async fn apply_catalog(
        &self,
        revision: u64,
        enabled: bool,
        tools: Vec<ResourceTool>,
    ) -> Result<()> {
        self.apply_catalog_with_guide(revision, enabled, tools, String::new())
            .await
    }

    /// 应用同一修订的目录与全局说明，并回收被撤销实例。
    ///
    /// # Errors
    /// 拒绝无效目录、说明或未完成的进程清理。
    pub async fn apply_catalog_with_guide(
        &self,
        revision: u64,
        enabled: bool,
        tools: Vec<ResourceTool>,
        guide_markdown: String,
    ) -> Result<()> {
        self.publish_catalog_with_guide(revision, enabled, tools, guide_markdown)?;
        let handles = {
            let mut instances = self.instances();
            let removed: Vec<_> = instances
                .iter()
                .filter(|(_, handle)| {
                    !self.identity_allowed(&handle.identity) || *handle.cancel.borrow()
                })
                .map(|(key, _)| key.clone())
                .collect();
            removed
                .into_iter()
                .filter_map(|key| instances.remove(&key))
                .inspect(|handle| {
                    handle.cancel.send_replace(true);
                })
                .collect::<Vec<_>>()
        };
        for handle in handles {
            cleanup_handle(&handle).await;
        }
        self.cleanup.finish().await
    }

    fn identity_allowed(&self, identity: &(String, String, String)) -> bool {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_none_or(|catalog| {
                catalog.enabled
                    && catalog.tools.iter().any(|tool| {
                        tool.id == identity.0
                            && tool.version == identity.1
                            && tool.package.sha256 == identity.2
                    })
            })
    }

    async fn current_tools(&self) -> Result<(url::Url, Vec<ResourceTool>, ResourceConfig)> {
        let centralized = self
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(catalog) = centralized {
            if !catalog.enabled {
                return Err(ResourceError::before("disabled", "中继工具中心已关闭"));
            }
            let origins = catalog
                .tools
                .iter()
                .filter_map(|tool| url::Url::parse(&tool.package.url).ok())
                .filter(|url| url.scheme() == "https")
                .map(|url| url.origin().ascii_serialization())
                .collect();
            return Ok((
                url::Url::parse("https://relay.invalid/remoteops/v1/index.json")
                    .expect("constant URL"),
                catalog.tools,
                ResourceConfig {
                    index_url: None,
                    allowed_origins: origins,
                },
            ));
        }
        let (url, index) = cache::fetch_index(&self.client, &self.config, &self.root).await?;
        Ok((url, index.tools, self.config.clone()))
    }

    /// 刷新清单并返回所有候选；`compatible()` 仅认可当前平台的通过组合。
    ///
    /// # Errors
    /// 来源、网络、清单或缓存校验失败。
    pub async fn list_tools(&self) -> Result<Vec<ResourceTool>> {
        self.list_catalog().await.map(|(_, tools)| tools)
    }

    /// 返回同一目录快照的全局说明与候选，移除下载 URL 的签名查询。
    ///
    /// # Errors
    /// 目录关闭或来源、清单、网络校验失败。
    pub async fn list_catalog(&self) -> Result<(String, Vec<ResourceTool>)> {
        let _guard = self.preparation.lock().await;
        let centralized = self
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let (guide, mut tools) = if let Some(catalog) = centralized {
            if !catalog.enabled {
                return Err(ResourceError::before("disabled", "中继工具中心已关闭"));
            }
            (catalog.guide_markdown, catalog.tools)
        } else {
            let (_, tools, _) = self.current_tools().await?;
            (String::new(), tools)
        };
        for tool in &mut tools {
            if let Ok(mut url) = url::Url::parse(&tool.package.url)
                && url.scheme() == "https"
            {
                url.set_query(None);
                url.set_fragment(None);
                tool.package.url = url.to_string();
            }
        }
        Ok((guide, tools))
    }

    /// 准备固定包并健康检查；成功前所有失败均未提交用户操作。
    ///
    /// # Errors
    /// 不兼容、包校验失败、实例数超限或启动/健康检查失败。
    pub async fn start(
        &self,
        session_id: &str,
        tool_id: &str,
        version: &str,
        sha256: &str,
    ) -> Result<StartedTool> {
        self.start_with_options(session_id, tool_id, version, sha256, false, None)
            .await
    }

    /// 按显式验证模式准备工具，进度回调仅发布脱敏状态。
    /// # Errors
    /// 平台策略、包校验、取消或健康检查失败。
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_options(
        &self,
        session_id: &str,
        tool_id: &str,
        version: &str,
        sha256: &str,
        validation_mode: bool,
        progress: Option<crate::ProgressReporter>,
    ) -> Result<StartedTool> {
        if let Some(p) = &progress {
            p.report("waiting", 0, None);
        }
        let (cancel, mut cancelled) = watch::channel(false);
        let registration = StartRegistration {
            key: Uuid::new_v4().to_string(),
            manager: self,
        };
        self.starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                registration.key.clone(),
                PendingStart {
                    session: session_id.into(),
                    identity: (tool_id.into(), version.into(), sha256.into()),
                    cancel,
                },
            );
        let result = tokio::select! {
            result = timeout(Duration::from_mins(15), self.start_inner(session_id, tool_id, version, sha256, cancelled.clone(), validation_mode, progress.clone())) => result.unwrap_or_else(|_| Err(ResourceError::before("startup_timeout", "工具准备超过15分钟，启动已取消"))),
            _ = cancelled.changed() => Err(ResourceError::before("startup_cancelled", "连接关闭或会话停止，资源启动已取消")),
        };
        if let Some(p) = &progress {
            p.report(
                if result.is_ok() {
                    "ready"
                } else if *cancelled.borrow() {
                    "cancelled"
                } else {
                    "failed"
                },
                0,
                None,
            );
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_inner(
        &self,
        session_id: &str,
        tool_id: &str,
        version: &str,
        sha256: &str,
        cancelled: watch::Receiver<bool>,
        validation_mode: bool,
        progress: Option<crate::ProgressReporter>,
    ) -> Result<StartedTool> {
        if session_id.is_empty() {
            return Err(ResourceError::before(
                "invalid_session",
                "必须绑定 RemoteOps 会话",
            ));
        }
        let guard = self.preparation.lock().await;
        if self.instances().len() >= MAX_INSTANCES {
            return Err(ResourceError::before(
                "instance_limit",
                "资源实例数量达到限制",
            ));
        }
        let (index_url, tools, config) = self.current_tools().await?;
        let tool = tools
            .into_iter()
            .find(|tool| {
                tool.id == tool_id && tool.version == version && tool.package.sha256 == sha256
            })
            .ok_or_else(|| {
                ResourceError::before("candidate_missing", "清单中没有匹配的工具版本和摘要")
            })?;
        tool.check_start_platform(validation_mode)?;
        if tool.requires_desktop && !desktop_available() {
            return Err(ResourceError::before(
                "desktop_required",
                "资源工具需要交互桌面",
            ));
        }
        let package = self
            .prepare_package(&config, &index_url, &tool, progress.clone())
            .await?;
        let run_root = self.root.join("runs");
        cache::prepare_root(&run_root)?;
        let run = tempfile::Builder::new()
            .prefix("session-")
            .tempdir_in(&run_root)
            .map_err(|_| ResourceError::before("runtime_io", "无法创建会话运行目录"))?;
        std::fs::create_dir(run.path().join("profile"))
            .map_err(|_| ResourceError::before("runtime_io", "无法创建独立浏览器 profile"))?;
        let seconds = tool.health_check.timeout_secs;
        let mut instance = Instance {
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
            cleanup: Arc::clone(&self.cleanup),
            #[cfg(windows)]
            job: None,
        };
        if let Some(p) = &progress {
            p.report("starting", 0, None);
        }
        instance.initialize_checked(seconds, &self.root).await?;
        let instance_id = Uuid::new_v4().to_string();
        let started = StartedTool {
            instance_id: instance_id.clone(),
            tool_id: instance.tool.id.clone(),
            version: instance.tool.version.clone(),
            sha256: instance.tool.package.sha256.clone(),
            tools: instance.tools.clone(),
        };
        let (cancel, _) = watch::channel(false);
        let mut instances = self.instances();
        if *cancelled.borrow()
            || !self.identity_allowed(&(tool_id.into(), version.into(), sha256.into()))
        {
            return Err(ResourceError::before(
                "startup_cancelled",
                "会话已停止，资源启动已取消",
            ));
        }
        instances.insert(
            instance_id,
            Arc::new(Handle {
                session: session_id.into(),
                identity: (tool_id.into(), version.into(), sha256.into()),
                cancel,
                instance: Mutex::new(Some(instance)),
            }),
        );
        drop(guard);
        Ok(started)
    }

    async fn prepare_package(
        &self,
        config: &ResourceConfig,
        index_url: &url::Url,
        tool: &ResourceTool,
        progress: Option<crate::ProgressReporter>,
    ) -> Result<PathBuf> {
        if let Some(p) = &progress {
            p.report("verify", 0, Some(tool.package.size));
        }
        if let Some(package) = cache::existing_package(&self.root, tool).await? {
            if let Some(p) = &progress {
                p.report("cached", tool.package.size, Some(tool.package.size));
            }
            Ok(package)
        } else if tool.package.url.starts_with("relay://") {
            let fetcher = self
                .package_fetcher
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .ok_or_else(|| {
                    ResourceError::before("download_unavailable", "中继包下载通道尚未就绪")
                })?;
            let expected_url = format!("relay://packages/{}.zip", tool.package.sha256);
            if tool.package.url != expected_url {
                return Err(ResourceError::before(
                    "source_denied",
                    "中继包地址与摘要不匹配",
                ));
            }
            let path = fetcher(tool.package.clone(), progress.clone()).await?;
            if !path.is_absolute() {
                return Err(ResourceError::before(
                    "invalid_download",
                    "中继下载必须返回绝对临时路径",
                ));
            }
            let archive = DownloadArchive(path);
            cache::imported_package_with_progress(archive.0.clone(), tool, &self.root, progress)
                .await
        } else {
            cache::package_with_progress(
                &self.client,
                config,
                index_url,
                tool,
                &self.root,
                progress,
            )
            .await
        }
    }

    /// 调用一次工具；取消、超时或读取失败会销毁实例，禁止自动重放。
    ///
    /// # Errors
    /// 会话错误或参数无效时尚未提交；提交后的失败返回 `result_unknown`。
    pub async fn call(
        &self,
        session_id: &str,
        instance_id: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value> {
        let handle = self
            .instances()
            .get(instance_id)
            .cloned()
            .ok_or_else(|| ResourceError {
                code: "instance_missing".into(),
                message: "资源实例已不存在；无法核实先前执行结果，禁止自动重放".into(),
                retry_safe: false,
                diagnostics: None,
            })?;
        if !self.identity_allowed(&handle.identity) {
            return Err(ResourceError::before("disabled", "中继已停用该工具"));
        }
        if handle.session != session_id {
            return Err(ResourceError::before(
                "session_mismatch",
                "资源实例属于其他会话",
            ));
        }
        let mut state = handle
            .instance
            .try_lock()
            .map_err(|_| ResourceError::before("instance_busy", "资源实例正在执行另一调用"))?;
        let mut instance = state.take().ok_or_else(ResourceError::unknown)?;
        if (!instance.tools.iter().any(|tool| tool.name == tool_name)
            && !matches!(
                tool_name,
                "__remoteops_prepare_password" | "__remoteops_fill_password"
            ))
            || !arguments.is_object()
            || serde_json::to_vec(&arguments).map_or(true, |bytes| bytes.len() > 1024 * 1024)
        {
            *state = Some(instance);
            return Err(ResourceError::before(
                "invalid_call",
                "工具名或 JSON 参数无效",
            ));
        }
        let mut cancel = handle.cancel.subscribe();
        if *cancel.borrow() {
            return Err(ResourceError::before("stopped", "资源实例已经停止"));
        }
        let mut active = ActiveCall {
            key: instance_id,
            manager: self,
            handle: &handle,
            armed: true,
        };
        let seconds = instance.tool.timeout_secs;
        let result = tokio::select! {
            result = timeout(Duration::from_secs(seconds), instance.call(tool_name, arguments)) => result.unwrap_or_else(|_| Err(ResourceError::unknown())),
            _ = cancel.changed() => Err(ResourceError::unknown()),
        };
        // Future 在任意 await 被取消时 instance 自动 Drop，state 保持 None。
        if result
            .as_ref()
            .map_or_else(|error| error.retry_safe, |_| true)
            && !*cancel.borrow()
        {
            *state = Some(instance);
            active.armed = false;
        } else {
            instance.terminate().await;
        }
        result
    }

    /// 停止当前会话的实例；不能停止其他会话。
    ///
    /// # Errors
    /// 实例存在但不属于当前会话。
    pub async fn stop(&self, session_id: &str, instance_id: &str) -> Result<()> {
        let handle = {
            let mut instances = self.instances();
            if let Some(handle) = instances.get(instance_id) {
                if handle.session != session_id {
                    return Err(ResourceError::before(
                        "session_mismatch",
                        "资源实例属于其他会话",
                    ));
                }
                let _ = handle.cancel.send(true);
            }
            instances.remove(instance_id)
        };
        if let Some(handle) = handle {
            cleanup_handle(&handle).await;
        }
        self.cleanup.finish().await
    }

    /// 回收某个 `RemoteOps` 会话的全部资源实例。
    ///
    /// # Errors
    /// 若启动仍未退出或临时目录仍被系统占用，返回清理未完成。
    pub async fn stop_session(&self, session_id: &str) -> Result<()> {
        self.cancel_starts(Some(session_id));
        let handles = {
            let mut instances = self.instances();
            let keys: Vec<_> = instances
                .iter()
                .filter(|(_, handle)| handle.session == session_id)
                .map(|(key, _)| key.clone())
                .collect();
            let handles: Vec<_> = keys
                .into_iter()
                .filter_map(|key| instances.remove(&key))
                .collect();
            for handle in &handles {
                let _ = handle.cancel.send(true);
            }
            handles
        };
        for handle in handles {
            cleanup_handle(&handle).await;
        }
        let starts = self.wait_starts(Some(session_id)).await;
        let cleanup = self.cleanup.finish().await;
        starts.and(cleanup)
    }

    /// 断线或 Agent 停止时回收所有资源实例。
    ///
    /// # Errors
    /// 若启动仍未退出或临时目录仍被系统占用，返回清理未完成。
    pub async fn stop_all(&self) -> Result<()> {
        self.cancel_starts(None);
        let handles = {
            let mut instances = self.instances();
            for handle in instances.values() {
                let _ = handle.cancel.send(true);
            }
            let handles: Vec<_> = instances.drain().map(|(_, value)| value).collect();
            handles
        };
        for handle in handles {
            cleanup_handle(&handle).await;
        }
        let starts = self.wait_starts(None).await;
        let cleanup = self.cleanup.finish().await;
        starts.and(cleanup)
    }
}

async fn cleanup_handle(handle: &Handle) {
    if let Some(mut instance) = handle.instance.lock().await.take() {
        instance.terminate().await;
    }
}

impl Instance {
    async fn initialize_checked(&mut self, seconds: u64, root: &Path) -> Result<()> {
        self.trace().launch(self.launch_context(false));
        let ready = timeout(Duration::from_secs(seconds), self.initialize()).await;
        let result = if let Ok(result) = ready {
            result
        } else {
            self.trace().reason("timeout");
            Err(ResourceError::before(
                "startup_timeout",
                "资源工具健康检查超时",
            ))
        };
        if let Err(mut error) = result {
            let expects_exit = self.trace().expects_exit();
            if expects_exit
                && let Some(process) = self.process.as_mut()
                && let Ok(Ok(status)) =
                    timeout(Duration::from_millis(100), process.child.inner_mut().wait()).await
            {
                self.trace().exit(status.code());
            }
            if let Some(process) = self.process.as_mut()
                && let Ok(Some(status)) = process.child.inner_mut().try_wait()
            {
                self.trace().exit(status.code());
            }
            // 先终止进程树，再给 stderr 有界的排空机会；诊断不依赖运行目录存活。
            drop(self.process.take());
            #[cfg(windows)]
            drop(self.job.take());
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut diagnostic = self.trace().snapshot(&[&self.package, self.run_path()]);
            diagnostics::save(root, &mut diagnostic);
            error.diagnostics = Some(Box::new(diagnostic));
            return Err(error);
        }
        self.trace().finish();
        Ok(())
    }

    fn trace(&self) -> MutexGuard<'_, diagnostics::Trace> {
        self.startup_trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn protocol_error(&self, submitted: bool, reason: &'static str) -> ResourceError {
        if !submitted {
            self.trace().reason(reason);
        }
        operation_error(submitted)
    }

    async fn terminate(&mut self) {
        // 先请求自有浏览器关闭，避免强杀时遗留 Windows 浏览器缓存写入句柄。
        match self.tool.startup.interface {
            ExecutionInterface::StdioMcp
                if self.tools.iter().any(|tool| tool.name == "browser_close") =>
            {
                let _ = timeout(
                    Duration::from_secs(2),
                    self.rpc(
                        "tools/call",
                        json!({"name":"browser_close","arguments":{}}),
                        false,
                    ),
                )
                .await;
            }
            ExecutionInterface::AgentBrowserCli => {
                if let Ok(mut command) = self.cli_command() {
                    command.arg("close");
                    let _ = timeout(Duration::from_secs(2), self.run_cli(command)).await;
                }
            }
            ExecutionInterface::StdioMcp | ExecutionInterface::Cli => {}
        }
        if let Some(mut process) = self.process.take() {
            #[cfg(windows)]
            {
                drop(process.job.take());
                drop(self.job.take());
            }
            let _ = process.child.start_kill();
            let _ = timeout(Duration::from_secs(10), process.child.wait()).await;
        }
        #[cfg(windows)]
        drop(self.job.take());
        if let Some(run) = self.run.take() {
            let path = run.keep();
            let cleanup = Arc::clone(&self.cleanup);
            let _ = tokio::task::spawn_blocking(move || {
                // Windows 的浏览器与病毒扫描器可能在进程退出后短暂持有 profile 文件。
                for _ in 0..100 {
                    if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                cleanup.schedule(path);
            })
            .await;
        }
    }
    fn run_path(&self) -> &Path {
        self.run
            .as_ref()
            .expect("live instance has run directory")
            .path()
    }

    fn command(&self) -> Result<Command> {
        self.command_executable(&self.tool.startup.executable)
    }

    fn command_executable(&self, executable: &str) -> Result<Command> {
        let mut command = Command::new(self.package.join(executable));
        command.current_dir(self.run_path());
        command.env_clear();
        // 只保留操作系统运行库所需环境，避免向第三方包泄漏宿主令牌。
        for name in [
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "LANG",
            "LC_ALL",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("PATH", PathBuf::from(root).join("System32"));
        }
        #[cfg(unix)]
        command.env("PATH", "/usr/bin:/bin");
        for name in [
            "HOME",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "TMPDIR",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            command.env(name, self.run_path());
        }
        command.env("AGENT_BROWSER_SOCKET_DIR", self.run_path().join("sockets"));
        for argument in &self.tool.startup.args {
            let value = argument
                .replace("${package}", &self.package.to_string_lossy())
                .replace("${run}", &self.run_path().to_string_lossy())
                .replace(
                    "${profile}",
                    &self.run_path().join("profile").to_string_lossy(),
                );
            if value.contains("${") {
                return Err(ResourceError::before(
                    "invalid_startup",
                    "启动参数含未知占位符",
                ));
            }
            command.arg(value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(command)
    }

    async fn initialize(&mut self) -> Result<()> {
        match self.tool.startup.interface {
            ExecutionInterface::StdioMcp => self.initialize_mcp().await?,
            ExecutionInterface::Cli => {
                crate::cli::validate_operations(&self.tool.startup.operations)?;
                self.tools = self
                    .tool
                    .startup
                    .operations
                    .iter()
                    .map(|operation| CallToolSchema {
                        name: operation.name.clone(),
                        description: operation.description.clone(),
                        input_schema: operation.input_schema.clone(),
                    })
                    .collect();
            }
            ExecutionInterface::AgentBrowserCli => {
                #[cfg(not(windows))]
                return Err(ResourceError::before(
                    "unsupported_platform",
                    "首版 agent-browser 受控进程树仅支持 Windows",
                ));
                #[cfg(windows)]
                {
                    self.job = Some(create_job()?);
                    self.trace().stage("cli_version");
                    let mut command = self.command()?;
                    command.arg("--version");
                    let result = self.run_cli(command).await.map_err(|_| {
                        ResourceError::before("startup_health", "agent-browser 版本检查失败")
                    })?;
                    if result.is_empty() {
                        return Err(ResourceError::before(
                            "startup_health",
                            "agent-browser 未返回版本",
                        ));
                    }
                    // 在返回启动成功前，实际验证独立浏览器能够打开；后续调用保持同一 Job。
                    self.trace().stage("browser_probe");
                    self.trace().launch(self.launch_context(true));
                    let mut command = self.cli_command()?;
                    command.args(["open", "about:blank"]);
                    self.run_cli_start(command).await.map_err(|_| {
                        ResourceError::before("startup_health", "agent-browser 独立浏览器启动失败")
                    })?;
                    self.tools = cli_schemas();
                }
            }
        }
        Ok(())
    }

    fn mcp_command(&self) -> Result<Command> {
        let mut command = self.command()?;
        match self.browser_adapter() {
            BrowserAdapter::Playwright => {
                command
                    .arg("--browser")
                    .arg("chromium")
                    .arg("--executable-path")
                    .arg(self.package.join(&self.tool.startup.browser_executable))
                    .arg("--user-data-dir")
                    .arg(self.run_path().join("profile"));
            }
            BrowserAdapter::AgentBrowser => {
                command.env(
                    "AGENT_BROWSER_EXECUTABLE_PATH",
                    self.package.join(&self.tool.startup.browser_executable),
                );
            }
            BrowserAdapter::None => {}
        }
        command.stderr(Stdio::piped());
        Ok(command)
    }

    fn browser_adapter(&self) -> BrowserAdapter {
        self.tool.startup.browser_adapter.unwrap_or({
            if self.tool.startup.browser_executable.is_empty() {
                BrowserAdapter::None
            } else {
                BrowserAdapter::Playwright
            }
        })
    }

    fn launch_context(&self, browser_probe: bool) -> diagnostics::LaunchContext {
        let (interface, adapter, source, args, env) = match self.tool.startup.interface {
            ExecutionInterface::StdioMcp => {
                let source = if self.tool.startup.browser_adapter.is_some() {
                    "explicit"
                } else {
                    "legacy_default"
                };
                match self.browser_adapter() {
                    BrowserAdapter::Playwright => (
                        "stdio_mcp",
                        Some("playwright"),
                        source,
                        vec![
                            "--browser",
                            "chromium",
                            "--executable-path",
                            "<browser>",
                            "--user-data-dir",
                            "<profile>",
                        ],
                        vec![],
                    ),
                    BrowserAdapter::AgentBrowser => (
                        "stdio_mcp",
                        Some("agent_browser"),
                        source,
                        vec![],
                        vec!["AGENT_BROWSER_EXECUTABLE_PATH"],
                    ),
                    BrowserAdapter::None => ("stdio_mcp", Some("none"), source, vec![], vec![]),
                }
            }
            ExecutionInterface::AgentBrowserCli => (
                "agent_browser_cli",
                None,
                "cli",
                if browser_probe {
                    vec![
                        "--json",
                        "--session",
                        "remoteops",
                        "--profile",
                        "<profile>",
                        "--executable-path",
                        "<browser>",
                        "open",
                        "about:blank",
                    ]
                } else {
                    vec!["--version"]
                },
                vec![],
            ),
            ExecutionInterface::Cli => ("cli", None, "cli", vec![], vec![]),
        };
        diagnostics::LaunchContext {
            interface: interface.into(),
            browser_adapter: adapter.map(str::to_owned),
            adapter_source: source.into(),
            injected_args: args.into_iter().map(str::to_owned).collect(),
            injected_env_names: std::iter::once("AGENT_BROWSER_SOCKET_DIR".to_owned())
                .chain(env.into_iter().map(str::to_owned))
                .collect(),
        }
    }

    async fn initialize_mcp(&mut self) -> Result<()> {
        self.trace().stage("spawn");
        let command = self.mcp_command()?;
        let mut process = spawn(command).inspect_err(|_| {
            self.trace().reason("spawn_failed");
        })?;
        if let Some(pipe) = process.child.stderr().take() {
            diagnostics::read_stderr(&self.startup_trace, pipe);
        }
        self.input = process.child.stdin().take();
        self.output = process.child.stdout().take().map(BufReader::new);
        self.process = Some(process);
        self.trace().stage("initialize");
        let initialized = self.rpc("initialize", json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"remoteops-resources","version":env!("CARGO_PKG_VERSION")}}), false).await?;
        self.trace().reason("invalid_initialize_response");
        if initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(ResourceError::before(
                "startup_protocol",
                "MCP 初始化响应无效",
            ));
        }
        self.trace().stage("initialized_notification");
        self.write(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            false,
        )
        .await?;
        self.trace().stage("tools_list");
        let tools = self.rpc("tools/list", json!({}), false).await?;
        self.trace().reason("invalid_tools_schema");
        let list = tools
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| ResourceError::before("startup_protocol", "MCP tools/list 响应无效"))?;
        if list.is_empty() || list.len() > 256 {
            return Err(ResourceError::before(
                "startup_protocol",
                "MCP 工具数量无效",
            ));
        }
        for tool in list {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|x| !x.is_empty() && x.len() <= 128)
                .ok_or_else(|| ResourceError::before("startup_protocol", "MCP 工具名无效"))?;
            let input_schema = tool
                .get("inputSchema")
                .filter(|x| x.is_object())
                .cloned()
                .ok_or_else(|| {
                    ResourceError::before("startup_protocol", "MCP 工具参数 Schema 无效")
                })?;
            self.tools.push(CallToolSchema {
                name: name.into(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                input_schema,
            });
        }
        if self.browser_adapter() == BrowserAdapter::Playwright {
            self.trace().stage("browser_probe");
            self.trace().reason("browser_tool_missing");
            if !self
                .tools
                .iter()
                .any(|tool| tool.name == "browser_navigate")
            {
                return Err(ResourceError::before(
                    "startup_protocol",
                    "首版 STDIO MCP 必须提供 Playwright browser_navigate",
                ));
            }
            // 探针只访问本实例的新 profile，不提交任何用户指定操作。
            let probe = self
                .rpc(
                    "tools/call",
                    json!({"name":"browser_navigate","arguments":{"url":"about:blank"}}),
                    false,
                )
                .await?;
            if probe.get("isError").and_then(Value::as_bool) == Some(true) {
                return Err(ResourceError::before(
                    "startup_health",
                    "Playwright 独立浏览器启动探针失败",
                ));
            }
        }
        Ok(())
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        if matches!(
            name,
            "__remoteops_prepare_password" | "__remoteops_fill_password"
        ) {
            return self.password_operation(name, arguments).await;
        }
        let mut result = self.call_regular(name, arguments).await?;
        self.redact_password_output(&mut result);
        Ok(result)
    }

    async fn call_regular(&mut self, name: &str, arguments: Value) -> Result<Value> {
        match self.tool.startup.interface {
            ExecutionInterface::Cli => self.call_generic_cli(name, &arguments).await,
            ExecutionInterface::StdioMcp => {
                let schema = self
                    .tools
                    .iter()
                    .find(|tool| tool.name == name)
                    .ok_or_else(|| ResourceError::before("invalid_call", "MCP 操作未登记"))?;
                let validator = jsonschema::validator_for(&schema.input_schema).map_err(|_| {
                    ResourceError::before("invalid_call", "MCP 参数 Schema 不受支持")
                })?;
                if !validator.is_valid(&arguments) {
                    return Err(ResourceError::before(
                        "invalid_call",
                        "MCP 参数不符合工具 Schema",
                    ));
                }
                self.rpc(
                    "tools/call",
                    json!({"name":name,"arguments":arguments}),
                    true,
                )
                .await
            }
            ExecutionInterface::AgentBrowserCli => {
                let args = cli_arguments(name, &arguments)?;
                let mut command = self.cli_command()?;
                command.args(args);
                let bytes = self.run_cli(command).await?;
                serde_json::from_slice(&bytes).map_err(|_| ResourceError::unknown())
            }
        }
    }

    async fn call_generic_cli(&self, name: &str, arguments: &Value) -> Result<Value> {
        let operation = self
            .tool
            .startup
            .operations
            .iter()
            .find(|operation| operation.name == name)
            .ok_or_else(|| ResourceError::before("invalid_call", "CLI 操作未登记"))?;
        let args = crate::cli::arguments(operation, arguments)?;
        let mut command = self.command_executable(
            operation
                .executable
                .as_deref()
                .unwrap_or(&self.tool.startup.executable),
        )?;
        command.args(args).stderr(Stdio::piped());
        let mut process = spawn(command)?;
        drop(process.child.stdin().take());
        let stdout = process
            .child
            .stdout()
            .take()
            .ok_or_else(ResourceError::unknown)?;
        let stderr = process
            .child
            .stderr()
            .take()
            .ok_or_else(ResourceError::unknown)?;
        let read = |pipe: Pin<Box<dyn tokio::io::AsyncRead + Send>>| async move {
            let mut bytes = Vec::new();
            pipe.take((MAX_OUTPUT + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| ResourceError::unknown())?;
            if bytes.len() > MAX_OUTPUT {
                return Err(ResourceError::unknown());
            }
            String::from_utf8(bytes).map_err(|_| ResourceError::unknown())
        };
        let (stdout, stderr) = tokio::try_join!(read(Box::pin(stdout)), read(Box::pin(stderr)))?;
        if stdout.len() + stderr.len() > MAX_OUTPUT {
            return Err(ResourceError::unknown());
        }
        let status = process
            .child
            .inner_mut()
            .wait()
            .await
            .map_err(|_| ResourceError::unknown())?;
        let result = match operation.result_format {
            crate::CliResultFormat::Text => Value::String(stdout.clone()),
            crate::CliResultFormat::Json => {
                serde_json::from_str(&stdout).map_err(|_| ResourceError::unknown())?
            }
        };
        Ok(
            json!({"exit_code": status.code(), "stdout": stdout, "stderr": stderr, "result": result}),
        )
    }

    fn cli_command(&self) -> Result<Command> {
        let mut command = self.command()?;
        command
            .arg("--json")
            .arg("--session")
            .arg("remoteops")
            .arg("--profile")
            .arg(self.run_path().join("profile"))
            .arg("--executable-path")
            .arg(self.package.join(&self.tool.startup.browser_executable));
        Ok(command)
    }

    async fn run_cli(&self, mut command: Command) -> Result<Vec<u8>> {
        command.stderr(Stdio::piped());
        #[cfg(windows)]
        let mut process = spawn_in_job(
            command,
            self.job.clone().ok_or_else(ResourceError::unknown)?,
        )?;
        #[cfg(not(windows))]
        let mut process = spawn(command)?;
        if let Some(pipe) = process.child.stderr().take() {
            diagnostics::read_stderr(&self.startup_trace, pipe);
        }
        drop(process.child.stdin().take());
        let output = process
            .child
            .stdout()
            .take()
            .ok_or_else(ResourceError::unknown)?;
        let mut bytes = Vec::new();
        output
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ResourceError::unknown())?;
        if bytes.len() > MAX_OUTPUT {
            return Err(ResourceError::unknown());
        }
        let status = process
            .child
            .inner_mut()
            .wait()
            .await
            .map_err(|_| ResourceError::unknown())?;
        self.trace().exit(status.code());
        if !status.success() {
            self.trace().reason("process_exit");
            return Err(ResourceError::unknown());
        }
        Ok(bytes)
    }

    #[cfg(windows)]
    async fn run_cli_start(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.stderr(Stdio::piped());
        let mut process = spawn_in_job(
            command,
            self.job.clone().ok_or_else(ResourceError::unknown)?,
        )?;
        if let Some(pipe) = process.child.stderr().take() {
            diagnostics::read_stderr(&self.startup_trace, pipe);
        }
        drop(process.child.stdin().take());
        let output = process
            .child
            .stdout()
            .take()
            .ok_or_else(ResourceError::unknown)?;
        let mut bytes = Vec::new();
        output
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ResourceError::unknown())?;
        if bytes.len() > MAX_OUTPUT {
            return Err(ResourceError::unknown());
        }
        let status = process
            .child
            .inner_mut()
            .wait()
            .await
            .map_err(|_| ResourceError::unknown())?;
        self.trace().exit(status.code());
        if !status.success() {
            self.trace().reason("process_exit");
            return Err(ResourceError::unknown());
        }
        self.process = Some(process);
        Ok(bytes)
    }

    async fn write(&mut self, message: Value, submitted: bool) -> Result<()> {
        if !submitted {
            self.trace().sent(&message);
        }
        let mut bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&message).map_err(|_| operation_error(submitted))?,
        );
        bytes.push(b'\n');
        self.input
            .as_mut()
            .ok_or_else(|| operation_error(submitted))?
            .write_all(&bytes)
            .await
            .map_err(|_| self.protocol_error(submitted, "write_failed"))
    }

    async fn rpc(&mut self, method: &str, params: Value, submitted: bool) -> Result<Value> {
        self.sequence += 1;
        let sequence = self.sequence;
        self.write(
            json!({"jsonrpc":"2.0","id":sequence,"method":method,"params":params}),
            submitted,
        )
        .await?;
        let mut consumed = 0;
        loop {
            let output = self
                .output
                .as_mut()
                .ok_or_else(|| operation_error(submitted))?;
            let mut bytes = Vec::new();
            let read = output
                .take((MAX_OUTPUT - consumed + 1) as u64)
                .read_until(b'\n', &mut bytes)
                .await
                .map_err(|_| self.protocol_error(submitted, "read_failed"))?;
            consumed += read;
            if read == 0 {
                return Err(self.protocol_error(submitted, "eof"));
            }
            if consumed > MAX_OUTPUT {
                return Err(self.protocol_error(submitted, "output_limit"));
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                self.trace().invalid_received(bytes.len());
                self.protocol_error(submitted, "invalid_json")
            })?;
            if !submitted {
                self.trace().received(&value);
            }
            if value.get("id").and_then(Value::as_u64) == Some(sequence) {
                if let Some(result) = value.get("result") {
                    return Ok(result.clone());
                }
                if value.get("error").is_some() {
                    if submitted {
                        return Ok(
                            json!({"isError":true,"content":[{"type":"text","text":"上游工具返回执行错误；请核实状态后决定后续操作"}]}),
                        );
                    }
                    self.trace().reason("upstream_error");
                    return Err(ResourceError::before(
                        "startup_protocol",
                        "MCP 上游拒绝初始化或健康检查",
                    ));
                }
                return Err(self.protocol_error(submitted, "invalid_response"));
            }
            // 首版不提供上游 sampling、elicitation 或 roots，明确拒绝服务端请求。
            if let Some(id) = value.get("id").filter(|_| value.get("method").is_some()) {
                self.write(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"RemoteOps does not expose client methods"}}), submitted).await?;
            }
        }
    }
}

fn operation_error(submitted: bool) -> ResourceError {
    if submitted {
        ResourceError::unknown()
    } else {
        ResourceError::before("startup_protocol", "资源工具初始化通信失败")
    }
}

fn desktop_available() -> bool {
    remoteops_native_host::collect().interactive_desktop == Some(true)
}

fn spawn(command: Command) -> Result<ManagedProcess> {
    #[cfg(windows)]
    {
        spawn_in_job(command, create_job()?)
    }
    #[cfg(unix)]
    {
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop).wrap(ProcessGroup::leader());
        command
            .spawn()
            .map(|child| ManagedProcess { child })
            .map_err(|_| ResourceError::before("spawn_failed", "资源可执行文件启动失败"))
    }
}

#[cfg(windows)]
fn create_job() -> Result<Arc<win32job::Job>> {
    let job = win32job::Job::create()
        .map_err(|_| ResourceError::before("spawn_failed", "无法创建资源进程 Job"))?;
    let mut limits = job
        .query_extended_limit_info()
        .map_err(|_| ResourceError::before("spawn_failed", "无法读取资源进程 Job 限制"))?;
    limits.limit_kill_on_job_close();
    job.set_extended_limit_info(&limits)
        .map_err(|_| ResourceError::before("spawn_failed", "无法设置资源进程 Job 限制"))?;
    Ok(Arc::new(job))
}

#[cfg(windows)]
fn spawn_in_job(mut command: Command, job: Arc<win32job::Job>) -> Result<ManagedProcess> {
    use process_wrap::tokio::{CommandWrap, CreationFlags, JobObject};
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    command.kill_on_drop(true);
    let mut command = CommandWrap::from(command);
    // JobObject 先以暂停状态创建进程；post_spawn 加入共享 Job 后才恢复线程。
    command
        .wrap(CreationFlags(CREATE_NO_WINDOW))
        .wrap(SharedJob(Arc::clone(&job)))
        .wrap(JobObject);
    let child = command
        .spawn()
        .map_err(|_| ResourceError::before("spawn_failed", "资源可执行文件启动或 Job 绑定失败"))?;
    Ok(ManagedProcess {
        child,
        job: Some(job),
    })
}

#[cfg(windows)]
#[derive(Debug)]
struct SharedJob(Arc<win32job::Job>);

#[cfg(windows)]
impl process_wrap::tokio::CommandWrapper for SharedJob {
    fn post_spawn(
        &mut self,
        _command: &mut Command,
        child: &mut tokio::process::Child,
        _core: &process_wrap::tokio::CommandWrap,
    ) -> std::io::Result<()> {
        let handle = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("missing suspended process handle"))?;
        self.0
            .assign_process(handle as isize)
            .map_err(|_| std::io::Error::other("job assignment failed"))
    }
}

fn cli_schemas() -> Vec<CallToolSchema> {
    [
        ("browser_open", "打开 URL", json!({"url":{"type":"string"}}), vec!["url"]),
        ("browser_snapshot", "获取页面语义快照", json!({}), vec![]),
        ("browser_click", "点击快照中的元素引用", json!({"ref":{"type":"string"}}), vec!["ref"]),
        ("browser_fill", "填写快照中的元素", json!({"ref":{"type":"string"},"text":{"type":"string"}}), vec!["ref","text"]),
        ("browser_eval", "在独立浏览器页面执行 JavaScript", json!({"script":{"type":"string"}}), vec!["script"]),
        ("browser_close", "关闭独立浏览器", json!({}), vec![]),
    ].into_iter().map(|(name, description, properties, required)| CallToolSchema { name: name.into(), description: description.into(), input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}) }).collect()
}

fn cli_arguments(name: &str, value: &Value) -> Result<Vec<String>> {
    let expected: &[&str] = match name {
        "browser_open" => &["url"],
        "browser_snapshot" | "browser_close" => &[],
        "browser_click" => &["ref"],
        "browser_fill" => &["ref", "text"],
        "browser_eval" => &["script"],
        _ => {
            return Err(ResourceError::before(
                "invalid_call",
                "不支持的 agent-browser 工具",
            ));
        }
    };
    if value.as_object().is_none_or(|fields| {
        fields.len() != expected.len() || fields.keys().any(|key| !expected.contains(&key.as_str()))
    }) {
        return Err(ResourceError::before(
            "invalid_call",
            "CLI 参数含未知字段或缺少必填字段",
        ));
    }
    let text = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .filter(|text| {
                !text.contains('\0') && text.len() <= 256 * 1024 && !text.starts_with('-')
            })
            .map(str::to_owned)
            .ok_or_else(|| ResourceError::before("invalid_call", "缺少有效的字符串参数"))
    };
    let reference = || {
        let value = text("ref")?;
        if value.starts_with('@')
            && value.len() <= 64
            && value[1..].bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            Ok(value)
        } else {
            Err(ResourceError::before(
                "invalid_call",
                "元素引用必须为 @ 开头的快照引用",
            ))
        }
    };
    match name {
        "browser_open" => {
            let url = text("url")?;
            if url::Url::parse(&url).is_err() || url.starts_with('-') {
                return Err(ResourceError::before("invalid_call", "URL 参数无效"));
            }
            Ok(vec!["open".into(), url])
        }
        "browser_snapshot" => Ok(vec!["snapshot".into(), "-i".into()]),
        "browser_click" => Ok(vec!["click".into(), reference()?]),
        "browser_fill" => Ok(vec!["fill".into(), reference()?, text("text")?]),
        "browser_eval" => Ok(vec!["eval".into(), text("script")?]),
        "browser_close" => Ok(vec!["close".into()]),
        _ => Err(ResourceError::before(
            "invalid_call",
            "不支持的 agent-browser 工具",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_never_takes_caller_profile_or_shell_arguments() {
        assert!(cli_arguments("browser_open", &json!({"url":"--profile"})).is_err());
        assert!(cli_arguments("browser_click", &json!({"ref":"--cdp"})).is_err());
        assert!(cli_arguments("browser_snapshot", &json!({"profile":"existing"})).is_err());
        assert!(cli_arguments("browser_fill", &json!({"ref":"@e1","text":"--profile=x"})).is_err());
        assert_eq!(
            cli_arguments("browser_fill", &json!({"ref":"@e1","text":"x; whoami"})).unwrap(),
            vec!["fill", "@e1", "x; whoami"]
        );
    }

    #[tokio::test]
    async fn manager_is_disabled_by_default_and_session_bound() {
        let directory = tempfile::tempdir().unwrap();
        let manager =
            ResourceManager::new(ResourceConfig::default(), directory.path().into()).unwrap();
        assert_eq!(manager.list_tools().await.unwrap_err().code, "disabled");
        assert!(
            !manager
                .call("a", "missing", "browser_open", json!({}))
                .await
                .unwrap_err()
                .retry_safe
        );
        manager.stop_all().await.unwrap();
    }
}
