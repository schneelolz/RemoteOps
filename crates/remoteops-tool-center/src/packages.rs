use std::{
    collections::HashMap,
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use url::Url;

use crate::{
    MAX_PACKAGE_SIZE, PACKAGE_CHUNK_SIZE, PackageReceipt, PackageSource, Result, error, storage,
    valid_sha,
};

pub(crate) struct Upload {
    pub actor: String,
    pub archive: tempfile::NamedTempFile,
    pub size: u64,
    pub sha256: String,
    pub offset: u64,
    pub updated: Instant,
}

pub(crate) fn expire(uploads: &mut HashMap<String, Upload>) {
    uploads.retain(|_, upload| upload.updated.elapsed() < Duration::from_mins(15));
}

pub(crate) fn begin(
    root: &Path,
    actor: &str,
    size: u64,
    sha256: &str,
    uploads: &mut HashMap<String, Upload>,
) -> Result<String> {
    expire(uploads);
    if size == 0 || size > MAX_PACKAGE_SIZE || !valid_sha(sha256) {
        return Err(error("invalid_package_identity", "包大小或 SHA-256 无效"));
    }
    if uploads.len() >= 8 || uploads.values().filter(|u| u.actor == actor).count() >= 2 {
        return Err(error("upload_limit", "并行上传数量超过限制"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    uploads.insert(
        id.clone(),
        Upload {
            actor: actor.into(),
            archive: tempfile::NamedTempFile::new_in(root).map_err(storage)?,
            size,
            sha256: sha256.into(),
            offset: 0,
            updated: Instant::now(),
        },
    );
    Ok(id)
}

pub(crate) fn chunk(upload: &mut Upload, actor: &str, offset: u64, encoded: &str) -> Result<u64> {
    if upload.actor != actor {
        return Err(error("upload_not_found", "上传不存在或不属于当前操作者"));
    }
    if encoded.len() > PACKAGE_CHUNK_SIZE.div_ceil(3) * 4 {
        return Err(error("chunk_limit", "分块超过 256 KiB"));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| error("invalid_chunk", "分块编码无效"))?;
    if bytes.len() > PACKAGE_CHUNK_SIZE {
        return Err(error("chunk_limit", "分块超过 256 KiB"));
    }
    if bytes.is_empty() || offset != upload.offset || offset + bytes.len() as u64 > upload.size {
        return Err(error("invalid_chunk", "分块偏移或长度无效"));
    }
    upload.archive.write_all(&bytes).map_err(storage)?;
    upload.offset += bytes.len() as u64;
    upload.updated = Instant::now();
    Ok(upload.offset)
}

pub(crate) fn hash_archive(path: &Path) -> Result<(u64, String)> {
    let metadata = std::fs::symlink_metadata(path).map_err(storage)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_PACKAGE_SIZE
    {
        return Err(error("invalid_package", "ZIP 文件无效或大小超过限制"));
    }
    let mut file = File::open(path).map_err(storage)?;
    let mut hash = Sha256::new();
    let mut bytes = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut bytes).map_err(storage)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_PACKAGE_SIZE {
            return Err(error("package_limit", "ZIP 文件超过 1 GiB"));
        }
        hash.update(&bytes[..read]);
    }
    Ok((total, format!("{:x}", hash.finalize())))
}

pub(crate) async fn validate(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        remoteops_resources::validate_archive(&path).map_err(|e| error(&e.code, &e.message))
    })
    .await
    .map_err(|_| error("package_worker_failed", "包校验工作线程失败"))?
}

pub(crate) async fn finish(mut upload: Upload, root: &Path) -> Result<PackageReceipt> {
    if upload.offset != upload.size {
        return Err(error("incomplete_upload", "上传尚未完成"));
    }
    upload.archive.flush().map_err(storage)?;
    let archive_path = upload.archive.path().to_path_buf();
    let (size, sha256) = tokio::task::spawn_blocking(move || hash_archive(&archive_path))
        .await
        .map_err(|_| error("package_worker_failed", "包摘要工作线程失败"))??;
    if size != upload.size || sha256 != upload.sha256 {
        return Err(error("hash_mismatch", "上传包的大小或 SHA-256 不符"));
    }
    validate(upload.archive.path().to_path_buf()).await?;
    let target = root.join(format!("{sha256}.zip"));
    if let Err(persist) = upload.archive.persist_noclobber(&target) {
        if !target.exists() {
            return Err(storage(persist.error));
        }
        let existing = target.clone();
        let actual = tokio::task::spawn_blocking(move || hash_archive(&existing))
            .await
            .map_err(|_| error("package_worker_failed", "包摘要工作线程失败"))??;
        if actual != (size, sha256.clone()) {
            return Err(error("package_corrupt", "已有同摘要包内容损坏"));
        }
    }
    Ok(PackageReceipt {
        size,
        sha256,
        file_verified: true,
        source: PackageSource::Hosted,
    })
}

pub(crate) fn external_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| error("invalid_external_url", "外部包 URL 无效"))?;
    if value.len() > 16 * 1024
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error(
            "invalid_external_url",
            "仅允许无用户信息的 HTTPS 包地址",
        ));
    }
    Ok(url)
}

pub(crate) async fn import_external(
    root: &Path,
    value: &str,
    expected: Option<&str>,
) -> Result<PackageReceipt> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_mins(10))
        .build()
        .map_err(|_| error("external_download_failed", "无法创建外链下载客户端"))?;
    import_external_with_client(root, value, expected, &client).await
}

pub(crate) async fn import_external_with_client(
    root: &Path,
    value: &str,
    expected: Option<&str>,
    client: &reqwest::Client,
) -> Result<PackageReceipt> {
    let url = external_url(value)?;
    if expected.is_some_and(|sha| !valid_sha(sha)) {
        return Err(error("invalid_package_identity", "包 SHA-256 无效"));
    }
    // 禁止重定向，避免签名参数跨来源泄露；过期地址返回固定脱敏错误。
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| error("external_download_failed", "外部 ZIP 请求失败"))?;
    if !response.status().is_success() {
        return Err(error(
            "external_http_status",
            "外部 ZIP 地址已过期或返回非成功状态",
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size == 0 || size > MAX_PACKAGE_SIZE)
    {
        return Err(error("package_limit", "外部 ZIP 大小超过限制"));
    }
    let archive = tempfile::NamedTempFile::new_in(root).map_err(storage)?;
    let mut output = tokio::fs::File::from_std(archive.reopen().map_err(storage)?);
    let mut size = 0_u64;
    let mut hash = Sha256::new();
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| error("external_download_failed", "读取外部 ZIP 失败"))?
    {
        size += bytes.len() as u64;
        if size > MAX_PACKAGE_SIZE {
            return Err(error("package_limit", "外部 ZIP 超过 1 GiB"));
        }
        hash.update(&bytes);
        output.write_all(&bytes).await.map_err(storage)?;
    }
    output.flush().await.map_err(storage)?;
    drop(output);
    let sha256 = format!("{:x}", hash.finalize());
    if size == 0 || expected.is_some_and(|expected| expected != sha256) {
        return Err(error("hash_mismatch", "外部 ZIP 的大小或 SHA-256 不符"));
    }
    validate(archive.path().to_path_buf()).await?;
    Ok(PackageReceipt {
        size,
        sha256,
        file_verified: true,
        source: PackageSource::External { url: value.into() },
    })
}
