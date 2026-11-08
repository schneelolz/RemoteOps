//! 已认证中继连接上的逐块下载与目录应用，包数据不进入无界队列。
use super::{
    Arc, BASE64, BTreeMap, Deserialize, Duration, Engine, JoinHandle, Mutex, PathBuf, RequestId,
    WireMessage, mpsc, timeout, watch,
};
use remoteops_resources::{ResourceError, ResourceManager, ResourcePackage, ResourceTool};
use tokio::{io::AsyncWriteExt as _, sync::oneshot};

const CHUNK: usize = 256 * 1024;
#[derive(Clone, Deserialize)]
struct Catalog {
    revision: u64,
    enabled: bool,
    #[serde(default)]
    guide_markdown: String,
    tools: Vec<ResourceTool>,
}
#[derive(Default)]
struct Downloads {
    pending: Mutex<BTreeMap<String, oneshot::Sender<WireMessage>>>,
}
pub(super) struct AgentToolTransport {
    downloads: Arc<Downloads>,
    catalog: watch::Sender<Option<Catalog>>,
    worker: JoinHandle<()>,
    resources: Option<Arc<ResourceManager>>,
}
fn error(code: &str, message: &str) -> ResourceError {
    ResourceError {
        code: code.into(),
        message: message.into(),
        retry_safe: true,
        diagnostics: None,
    }
}
impl AgentToolTransport {
    pub(super) fn attach(
        resources: Option<Arc<ResourceManager>>,
        sender: mpsc::UnboundedSender<WireMessage>,
    ) -> Self {
        let downloads = Arc::new(Downloads::default());
        let (catalog, mut receiver) = watch::channel::<Option<Catalog>>(None);
        if let Some(manager) = &resources {
            manager.reset_catalog();
            let downloads = downloads.clone();
            let sender = sender.clone();
            manager.set_package_fetcher(Arc::new(move |package, progress| {
                let downloads = downloads.clone();
                let sender = sender.clone();
                Box::pin(async move {
                    downloads
                        .fetch_with_progress(package, sender, progress)
                        .await
                })
            }));
        }
        let manager = resources.clone();
        let worker = tokio::spawn(async move {
            while receiver.changed().await.is_ok() {
                let next = receiver.borrow_and_update().clone();
                if let (Some(next), Some(manager)) = (next, manager.as_ref()) {
                    let result = manager
                        .apply_catalog_with_guide(
                            next.revision,
                            next.enabled,
                            next.tools,
                            next.guide_markdown,
                        )
                        .await;
                    let _ = sender.send(WireMessage::ToolCatalogApplied {
                        revision: next.revision,
                        error: result.err().map(|e| e.code),
                    });
                }
            }
        });
        Self {
            downloads,
            catalog,
            worker,
            resources,
        }
    }
    pub(super) async fn handle(&self, message: WireMessage) {
        match message {
            WireMessage::ToolCatalog { catalog } => {
                if let Ok(catalog) = serde_json::from_value::<Catalog>(catalog) {
                    if let Some(manager) = &self.resources
                        && manager
                            .publish_catalog_with_guide(
                                catalog.revision,
                                catalog.enabled,
                                catalog.tools.clone(),
                                catalog.guide_markdown.clone(),
                            )
                            .is_err()
                    {
                        return;
                    }
                    self.catalog.send_replace(Some(catalog));
                }
            }
            message @ WireMessage::ToolPackageChunk { .. } => {
                let request_id = match &message {
                    WireMessage::ToolPackageChunk { request_id, .. } => request_id.clone(),
                    _ => unreachable!(),
                };
                if let Some(sender) = self.downloads.pending.lock().await.remove(&request_id) {
                    let _ = sender.send(message);
                }
            }
            _ => {}
        }
    }
    pub(super) async fn close(self) {
        self.worker.abort();
        let _ = self.worker.await;
        self.downloads.pending.lock().await.clear();
        if let Some(manager) = &self.resources {
            // 重连必须重新取得本次中继目录；旧快照不能用于离线启动。
            manager.reset_catalog();
            let _ = manager.stop_all().await;
        }
    }
}
impl Downloads {
    #[cfg(test)]
    async fn fetch(
        &self,
        package: ResourcePackage,
        sender: mpsc::UnboundedSender<WireMessage>,
    ) -> std::result::Result<PathBuf, ResourceError> {
        self.fetch_with_progress(package, sender, None).await
    }
    async fn fetch_with_progress(
        &self,
        package: ResourcePackage,
        sender: mpsc::UnboundedSender<WireMessage>,
        progress: Option<remoteops_resources::ProgressReporter>,
    ) -> std::result::Result<PathBuf, ResourceError> {
        if let Some(p) = &progress {
            p.report("download", 0, Some(package.size));
        }
        let temporary = tempfile::NamedTempFile::new()
            .map_err(|_| error("download_io", "无法创建包接收临时文件"))?
            .into_temp_path();
        let mut output = tokio::fs::File::create(&temporary)
            .await
            .map_err(|_| error("download_io", "无法写入包接收文件"))?;
        let mut offset = 0;
        loop {
            let request_id = RequestId::new().to_string();
            let (response, receiver) = oneshot::channel();
            self.pending
                .lock()
                .await
                .retain(|_, sender| !sender.is_closed());
            self.pending
                .lock()
                .await
                .insert(request_id.clone(), response);
            sender
                .send(WireMessage::ToolPackageRequest {
                    request_id: request_id.clone(),
                    sha256: package.sha256.clone(),
                    offset,
                })
                .map_err(|_| error("download_disconnected", "中继下载连接已断开"))?;
            let result = timeout(Duration::from_secs(30), receiver).await;
            self.pending.lock().await.remove(&request_id);
            let message = result
                .ok()
                .and_then(Result::ok)
                .ok_or_else(|| error("download_disconnected", "等待包分块确认失败"))?;
            let WireMessage::ToolPackageChunk {
                offset: chunk_offset,
                data_base64,
                eof,
                error: failure,
                ..
            } = message
            else {
                return Err(error("download_protocol", "包分块响应无效"));
            };
            if failure.is_some() {
                return Err(error("download_denied", "工具已停用或托管包不可用"));
            }
            if data_base64.len() > CHUNK.div_ceil(3) * 4 {
                return Err(error("download_limit", "包分块超过限制"));
            }
            let bytes = BASE64
                .decode(data_base64)
                .map_err(|_| error("download_protocol", "包分块编码无效"))?;
            if chunk_offset != offset
                || bytes.len() > CHUNK
                || (!eof && bytes.len() != CHUNK)
                || offset + bytes.len() as u64 > package.size
            {
                return Err(error("download_protocol", "包分块偏移或长度无效"));
            }
            output
                .write_all(&bytes)
                .await
                .map_err(|_| error("download_io", "无法保存包分块"))?;
            offset += bytes.len() as u64;
            if let Some(p) = &progress {
                p.report("download", offset, Some(package.size));
            }
            if eof {
                if offset != package.size {
                    return Err(error("size_mismatch", "托管包大小不符"));
                }
                output
                    .flush()
                    .await
                    .map_err(|_| error("download_io", "无法提交包接收文件"))?;
                drop(output);
                return temporary
                    .keep()
                    .map_err(|_| error("download_io", "无法保留已接收包"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(size: u64) -> ResourcePackage {
        ResourcePackage {
            url: format!("relay://packages/{}.zip", "a".repeat(64)),
            size,
            sha256: "a".repeat(64),
            format: "zip".into(),
        }
    }

    async fn requested(receiver: &mut mpsc::UnboundedReceiver<WireMessage>) -> (String, u64) {
        match timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap()
        {
            WireMessage::ToolPackageRequest {
                request_id, offset, ..
            } => (request_id, offset),
            other => panic!("意外传输消息: {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_waits_for_each_ack_and_reassembles_bounded_chunks() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let transport = AgentToolTransport::attach(None, sender.clone());
        let downloads = transport.downloads.clone();
        let download =
            tokio::spawn(
                async move { downloads.fetch(package((CHUNK + 17) as u64), sender).await },
            );
        let (request_id, offset) = requested(&mut receiver).await;
        assert_eq!(offset, 0);
        assert!(receiver.try_recv().is_err(), "上一块确认前不能请求下一块");
        transport
            .handle(WireMessage::ToolPackageChunk {
                request_id,
                offset,
                data_base64: BASE64.encode(vec![7; CHUNK]),
                eof: false,
                error: None,
            })
            .await;
        let (request_id, offset) = requested(&mut receiver).await;
        assert_eq!(offset, CHUNK as u64);
        transport
            .handle(WireMessage::ToolPackageChunk {
                request_id,
                offset,
                data_base64: BASE64.encode([9; 17]),
                eof: true,
                error: None,
            })
            .await;
        let path = download.await.unwrap().unwrap();
        let contents = tokio::fs::read(&path).await.unwrap();
        assert_eq!(contents.len(), CHUNK + 17);
        assert!(contents[..CHUNK].iter().all(|byte| *byte == 7));
        assert_eq!(contents[CHUNK..], [9; 17]);
        tokio::fs::remove_file(path).await.unwrap();
        assert!(transport.downloads.pending.lock().await.is_empty());
        transport.close().await;
    }

    #[tokio::test]
    async fn download_rejects_offsets_oversized_short_truncated_and_denied_chunks() {
        for (offset, size, eof, failure, expected) in [
            (1, CHUNK, true, None, "download_protocol"),
            (0, CHUNK + 3, true, None, "download_limit"),
            (0, 1, false, None, "download_protocol"),
            (0, 1, true, None, "size_mismatch"),
            (0, 0, true, Some("tool_disabled"), "download_denied"),
        ] {
            let (sender, mut receiver) = mpsc::unbounded_channel();
            let transport = AgentToolTransport::attach(None, sender.clone());
            let downloads = transport.downloads.clone();
            let download =
                tokio::spawn(async move { downloads.fetch(package(CHUNK as u64), sender).await });
            let (request_id, _) = requested(&mut receiver).await;
            transport
                .handle(WireMessage::ToolPackageChunk {
                    request_id,
                    offset,
                    data_base64: BASE64.encode(vec![0; size]),
                    eof,
                    error: failure.map(str::to_owned),
                })
                .await;
            let failure = download.await.unwrap().unwrap_err();
            assert_eq!(failure.code, expected);
            assert!(failure.retry_safe, "尚未启动工具的失败可安全重新选择候选");
            assert!(transport.downloads.pending.lock().await.is_empty());
            transport.close().await;
        }
    }

    #[tokio::test]
    async fn disconnect_releases_pending_download_without_waiting_for_timeout() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let transport = AgentToolTransport::attach(None, sender.clone());
        let downloads = transport.downloads.clone();
        let download =
            tokio::spawn(async move { downloads.fetch(package(CHUNK as u64), sender).await });
        let _ = requested(&mut receiver).await;
        transport.close().await;
        let error = timeout(Duration::from_secs(3), download)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "download_disconnected");
    }

    #[tokio::test]
    async fn catalog_gate_changes_in_reader_and_reconnect_drops_previous_revision() {
        let root = tempfile::tempdir().unwrap();
        let manager = Arc::new(ResourceManager::new_centralized(root.path().into()).unwrap());
        manager.publish_catalog(99, true, vec![]).unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let transport = AgentToolTransport::attach(Some(manager.clone()), sender);
        assert_eq!(manager.catalog_revision(), Some(0));
        assert_eq!(manager.list_tools().await.unwrap_err().code, "disabled");
        for (revision, enabled) in [(1, true), (2, false), (3, true)] {
            transport
                .handle(WireMessage::ToolCatalog {
                    catalog: serde_json::json!({"revision":revision,"enabled":enabled,"tools":[],"guide_markdown":"# guide"}),
                })
                .await;
            assert_eq!(
                manager.catalog_revision(),
                Some(revision),
                "读取任务必须立即发布每个修订"
            );
            assert_eq!(manager.list_tools().await.is_ok(), enabled);
            if enabled {
                assert_eq!(manager.list_catalog().await.unwrap().0, "# guide");
            }
        }
        transport
            .handle(WireMessage::ToolCatalog {
                catalog: serde_json::json!({"revision":2,"enabled":false,"tools":[]}),
            })
            .await;
        assert_eq!(manager.catalog_revision(), Some(3));
        let applied = timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            applied,
            WireMessage::ToolCatalogApplied {
                revision: 3,
                error: None
            }
        ));
        transport.close().await;
        assert_eq!(manager.catalog_revision(), Some(0));
        assert_eq!(manager.list_tools().await.unwrap_err().code, "disabled");
    }
}
