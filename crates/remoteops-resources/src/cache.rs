use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    ResourceConfig, ResourceError, ResourceIndex, ResourceTool, Result, manifest::safe_relative,
    validate_manifest,
};

const MAX_INDEX_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const RECEIPT: &str = ".remoteops-integrity.json";

#[derive(Default, Serialize, Deserialize)]
struct CachedIndex {
    url: String,
    #[serde(default)]
    resolved_url: String,
    etag: Option<String>,
    modified: Option<String>,
    bytes: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    archive_sha256: String,
    files: BTreeMap<String, String>,
}

pub(crate) fn client() -> Result<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_mins(15))
        .build()
        .map_err(|_| ResourceError::before("network_setup", "无法初始化 HTTPS 客户端"))
}

async fn get(
    client: &Client,
    config: &ResourceConfig,
    mut url: Url,
    etag: Option<&str>,
    modified: Option<&str>,
) -> Result<reqwest::Response> {
    for hop in 0..=5 {
        config.check_url(&url)?;
        let mut request = client.get(url.clone());
        if hop == 0 {
            if let Some(value) = etag {
                request = request.header(header::IF_NONE_MATCH, value);
            }
            if let Some(value) = modified {
                request = request.header(header::IF_MODIFIED_SINCE, value);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|_| ResourceError::before("download_failed", "HTTPS 资源请求失败"))?;
        if response.status().is_redirection() && response.status() != StatusCode::NOT_MODIFIED {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|x| x.to_str().ok())
                .ok_or_else(|| ResourceError::before("redirect_denied", "重定向缺少有效地址"))?;
            url = url
                .join(location)
                .map_err(|_| ResourceError::before("redirect_denied", "重定向地址无效"))?;
            continue;
        }
        if !response.status().is_success() && response.status() != StatusCode::NOT_MODIFIED {
            return Err(ResourceError::before("http_status", "资源站返回非成功状态"));
        }
        return Ok(response);
    }
    Err(ResourceError::before(
        "redirect_limit",
        "资源重定向次数超过限制",
    ))
}

async fn bounded_bytes(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ResourceError::before("download_limit", "下载长度超过限制"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ResourceError::before("download_failed", "读取 HTTPS 响应失败"))?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(ResourceError::before("download_limit", "下载长度超过限制"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn fetch_index(
    client: &Client,
    config: &ResourceConfig,
    root: &Path,
) -> Result<(Url, ResourceIndex)> {
    let url = config.index()?;
    prepare_root(root)?;
    let cache_path = root.join("index-cache.json");
    let cached: CachedIndex = fs::read(&cache_path)
        .ok()
        .filter(|x| x.len() <= 3 * MAX_INDEX_BYTES)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let resolved_cache = Url::parse(&cached.resolved_url)
        .ok()
        .filter(|resolved| config.check_url(resolved).is_ok());
    let trusted_cache = cached.url == url.as_str()
        && validate_manifest(&cached.bytes).is_ok()
        && resolved_cache.is_some();
    let response = get(
        client,
        config,
        url.clone(),
        trusted_cache.then_some(cached.etag.as_deref()).flatten(),
        trusted_cache
            .then_some(cached.modified.as_deref())
            .flatten(),
    )
    .await?;
    if response.status() == StatusCode::NOT_MODIFIED {
        if !trusted_cache {
            return Err(ResourceError::before(
                "invalid_cache",
                "资源站返回了无对应缓存的 304",
            ));
        }
        return Ok((
            resolved_cache.expect("trusted cache has checked response URL"),
            validate_manifest(&cached.bytes)?,
        ));
    }
    let resolved_url = response.url().clone();
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|x| x.to_str().ok())
        .map(str::to_owned);
    let modified = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|x| x.to_str().ok())
        .map(str::to_owned);
    let bytes = bounded_bytes(response, MAX_INDEX_BYTES).await?;
    let index = validate_manifest(&bytes)?;
    let entry = CachedIndex {
        url: url.as_str().into(),
        resolved_url: resolved_url.as_str().into(),
        etag,
        modified,
        bytes,
    };
    let serialized = serde_json::to_vec(&entry)
        .map_err(|_| ResourceError::before("cache_write", "无法序列化资源缓存"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(root).map_err(io_error)?;
    temporary.write_all(&serialized).map_err(io_error)?;
    temporary
        .persist(&cache_path)
        .map_err(|_| ResourceError::before("cache_write", "无法原子提交清单缓存"))?;
    Ok((resolved_url, index))
}

#[cfg(test)]
pub(crate) async fn package(
    client: &Client,
    config: &ResourceConfig,
    index_url: &Url,
    tool: &ResourceTool,
    root: &Path,
) -> Result<PathBuf> {
    package_with_progress(client, config, index_url, tool, root, None).await
}

pub(crate) async fn package_with_progress(
    client: &Client,
    config: &ResourceConfig,
    index_url: &Url,
    tool: &ResourceTool,
    root: &Path,
    progress: Option<crate::ProgressReporter>,
) -> Result<PathBuf> {
    prepare_root(root)?;
    let destination = root.join(&tool.package.sha256);
    if destination.exists() {
        verify_async(destination.clone(), tool.package.sha256.clone()).await?;
        return Ok(destination);
    }
    let url = index_url
        .join(&tool.package.url)
        .map_err(|_| ResourceError::before("invalid_manifest", "包地址无效"))?;
    let mut response = get(client, config, url, None, None).await?;
    if response
        .content_length()
        .is_some_and(|size| size != tool.package.size)
    {
        return Err(ResourceError::before("size_mismatch", "包长度与清单不符"));
    }
    let mut archive = tempfile::NamedTempFile::new_in(root).map_err(io_error)?;
    if let Some(p) = &progress {
        p.report("download", 0, Some(tool.package.size));
    }
    let mut downloaded = 0_u64;
    let mut hash = Sha256::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ResourceError::before("download_failed", "读取资源包失败"))?
    {
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ResourceError::before("download_limit", "包长度溢出"))?;
        if downloaded > tool.package.size {
            return Err(ResourceError::before(
                "download_limit",
                "下载包超过声明大小",
            ));
        }
        if let Some(p) = &progress {
            p.report("download", downloaded, Some(tool.package.size));
        }
        hash.update(&chunk);
        archive.write_all(&chunk).map_err(io_error)?;
    }
    if let Some(p) = &progress {
        p.report("verify", downloaded, Some(tool.package.size));
    }
    if downloaded != tool.package.size {
        return Err(ResourceError::before("size_mismatch", "包长度与清单不符"));
    }
    if format!("{:x}", hash.finalize()) != tool.package.sha256 {
        return Err(ResourceError::before(
            "hash_mismatch",
            "包 SHA-256 与清单不符",
        ));
    }
    archive.flush().map_err(io_error)?;
    if let Some(p) = &progress {
        p.report("extract", 0, Some(tool.package.size));
    }
    commit_archive(archive, tool, root, destination).await
}

pub(crate) async fn existing_package(root: &Path, tool: &ResourceTool) -> Result<Option<PathBuf>> {
    let destination = root.join(&tool.package.sha256);
    if destination.exists() {
        verify_async(destination.clone(), tool.package.sha256.clone()).await?;
        return Ok(Some(destination));
    }
    Ok(None)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn imported_package(
    path: PathBuf,
    tool: &ResourceTool,
    root: &Path,
) -> Result<PathBuf> {
    imported_package_with_progress(path, tool, root, None).await
}

pub(crate) async fn imported_package_with_progress(
    path: PathBuf,
    tool: &ResourceTool,
    root: &Path,
    progress: Option<crate::ProgressReporter>,
) -> Result<PathBuf> {
    if let Some(p) = &progress {
        p.report("verify", 0, Some(tool.package.size));
    }
    prepare_root(root)?;
    let destination = root.join(&tool.package.sha256);
    if destination.exists() {
        verify_async(destination.clone(), tool.package.sha256.clone()).await?;
        return Ok(destination);
    }
    let root_owned = root.to_path_buf();
    let expected_size = tool.package.size;
    let expected_hash = tool.package.sha256.clone();
    let archive = tokio::task::spawn_blocking(move || {
        reject_link(&path)?;
        let mut input = File::open(&path).map_err(io_error)?;
        if input.metadata().map_err(io_error)?.len() != expected_size
            || expected_size > 1024 * 1024 * 1024
        {
            return Err(ResourceError::before("size_mismatch", "包长度与目录不符"));
        }
        let mut archive = tempfile::NamedTempFile::new_in(root_owned).map_err(io_error)?;
        let copied = std::io::copy(
            &mut std::io::Read::by_ref(&mut input).take(expected_size + 1),
            &mut archive,
        )
        .map_err(io_error)?;
        if copied != expected_size || hash_file(archive.path())? != expected_hash {
            return Err(ResourceError::before("hash_mismatch", "包摘要与目录不符"));
        }
        Ok(archive)
    })
    .await
    .map_err(|_| ResourceError::before("cache_worker", "包导入工作线程失败"))??;
    if let Some(p) = &progress {
        p.report("extract", 0, Some(tool.package.size));
    }
    commit_archive(archive, tool, root, destination).await
}

async fn commit_archive(
    mut archive: tempfile::NamedTempFile,
    tool: &ResourceTool,
    root: &Path,
    destination: PathBuf,
) -> Result<PathBuf> {
    archive.flush().map_err(io_error)?;
    let staging = tempfile::Builder::new()
        .prefix("unpack-")
        .tempdir_in(root)
        .map_err(io_error)?;
    let digest = tool.package.sha256.clone();
    let (archive, staging) = tokio::task::spawn_blocking(move || {
        extract(archive.path(), staging.path(), &digest)?;
        Ok::<_, ResourceError>((archive, staging))
    })
    .await
    .map_err(|_| ResourceError::before("cache_worker", "资源解压工作线程失败"))??;
    for required in [&tool.startup.executable, &tool.startup.browser_executable]
        .into_iter()
        .map(String::as_str)
        .chain(
            tool.startup
                .operations
                .iter()
                .filter_map(|operation| operation.executable.as_deref()),
        )
        .filter(|value| !value.is_empty())
    {
        if !staging.path().join(required).is_file() {
            return Err(ResourceError::before(
                "invalid_package",
                "包内缺少声明的可执行文件",
            ));
        }
    }
    let archive_destination = destination.with_extension("zip");
    if let Err(error) = archive.persist_noclobber(&archive_destination) {
        if !archive_destination.exists() {
            return Err(io_error(error));
        }
        reject_link(&archive_destination)?;
    }
    // 不覆盖已有目录：不同进程的竞态只复核赢家提交的固定摘要缓存。
    match fs::rename(staging.path(), &destination) {
        Ok(()) => {
            let _ = staging.keep();
        }
        Err(_) if destination.exists() => {
            verify_async(destination.clone(), tool.package.sha256.clone()).await?;
        }
        Err(_) => {
            return Err(ResourceError::before(
                "cache_write",
                "无法原子提交资源包缓存",
            ));
        }
    }
    verify_async(destination.clone(), tool.package.sha256.clone()).await?;
    Ok(destination)
}

pub(crate) fn extract(archive: &Path, target: &Path, digest: &str) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive).map_err(io_error)?)
        .map_err(|_| ResourceError::before("invalid_zip", "无法读取 ZIP 目录"))?;
    if archive.len() > MAX_FILES {
        return Err(ResourceError::before("zip_limit", "ZIP 文件数量超过限制"));
    }
    let mut total = 0_u64;
    let mut paths = std::collections::HashSet::new();
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| ResourceError::before("invalid_zip", "无法读取 ZIP 条目"))?;
        let raw = entry.name().trim_end_matches('/').to_owned();
        if !safe_relative(&raw)
            || raw.eq_ignore_ascii_case(RECEIPT)
            || !paths.insert(raw.to_ascii_lowercase())
            || entry.unix_mode().is_some_and(|mode| {
                (mode & 0o170_000) != 0 && !matches!(mode & 0o170_000, 0o100_000 | 0o040_000)
            })
        {
            return Err(ResourceError::before(
                "unsafe_zip",
                "ZIP 含不安全路径、链接或重复条目",
            ));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| ResourceError::before("zip_limit", "ZIP 大小溢出"))?;
        if total > MAX_EXPANDED_BYTES
            || entry.size() > entry.compressed_size().max(1).saturating_mul(500)
        {
            return Err(ResourceError::before(
                "zip_limit",
                "ZIP 超过解压大小或压缩比限制",
            ));
        }
        let path = target.join(&raw);
        if entry.is_dir() {
            fs::create_dir_all(&path).map_err(io_error)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io_error)?;
        let entry_size = entry.size();
        let copied = std::io::copy(&mut entry.by_ref().take(entry_size + 1), &mut output)
            .map_err(io_error)?;
        if copied != entry.size() {
            return Err(ResourceError::before(
                "zip_limit",
                "ZIP 实际长度与条目声明不符",
            ));
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o755))
                .map_err(io_error)?;
        }
        files.insert(raw, hash_file(&path)?);
    }
    let receipt = Receipt {
        archive_sha256: digest.into(),
        files,
    };
    fs::write(
        target.join(RECEIPT),
        serde_json::to_vec(&receipt)
            .map_err(|_| ResourceError::before("cache_write", "完整性记录序列化失败"))?,
    )
    .map_err(io_error)?;
    Ok(())
}

pub(crate) fn verify(root: &Path, digest: &str) -> Result<()> {
    reject_link(root)?;
    let archive_path = root.with_extension("zip");
    reject_link(&archive_path)?;
    if hash_file(&archive_path)? != digest {
        return Err(ResourceError::before("cache_corrupt", "缓存原始包摘要不符"));
    }
    let expected = archive_hashes(&archive_path)?;
    let receipt_path = root.join(RECEIPT);
    reject_link(&receipt_path)?;
    let metadata = fs::metadata(&receipt_path).map_err(io_error)?;
    if metadata.len() > 32 * 1024 * 1024 {
        return Err(ResourceError::before(
            "cache_corrupt",
            "缓存完整性记录超过限制",
        ));
    }
    let receipt: Receipt = serde_json::from_slice(&fs::read(receipt_path).map_err(io_error)?)
        .map_err(|_| ResourceError::before("cache_corrupt", "缓存完整性记录无效"))?;
    if receipt.archive_sha256 != digest || receipt.files.len() > MAX_FILES {
        return Err(ResourceError::before("cache_corrupt", "缓存包身份无效"));
    }
    let mut actual = BTreeMap::new();
    walk(root, root, &mut actual, &mut 0)?;
    if actual != receipt.files || actual != expected {
        return Err(ResourceError::before(
            "cache_corrupt",
            "缓存文件内容已损坏或被修改，请人工移除该摘要缓存后重试",
        ));
    }
    Ok(())
}

async fn verify_async(root: PathBuf, digest: String) -> Result<()> {
    tokio::task::spawn_blocking(move || verify(&root, &digest))
        .await
        .map_err(|_| ResourceError::before("cache_worker", "资源完整性工作线程失败"))?
}

fn archive_hashes(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut archive = zip::ZipArchive::new(File::open(path).map_err(io_error)?)
        .map_err(|_| ResourceError::before("cache_corrupt", "缓存原始 ZIP 无效"))?;
    if archive.len() > MAX_FILES {
        return Err(ResourceError::before("zip_limit", "缓存条目超限"));
    }
    let mut expected = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io_error)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        if !safe_relative(&name) {
            return Err(ResourceError::before(
                "cache_corrupt",
                "缓存原始 ZIP 路径无效",
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = entry.read(&mut buffer).map_err(io_error)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > MAX_EXPANDED_BYTES {
                return Err(ResourceError::before("zip_limit", "缓存解压总量超限"));
            }
            hash.update(&buffer[..read]);
        }
        expected.insert(name, format!("{:x}", hash.finalize()));
    }
    Ok(expected)
}

fn walk(
    root: &Path,
    path: &Path,
    output: &mut BTreeMap<String, String>,
    visited: &mut usize,
) -> Result<()> {
    reject_link(path)?;
    for entry in fs::read_dir(path).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        *visited += 1;
        if *visited > MAX_FILES
            || path
                .strip_prefix(root)
                .map_err(io_error)?
                .components()
                .count()
                > 32
        {
            return Err(ResourceError::before(
                "cache_corrupt",
                "缓存目录条目或深度超限",
            ));
        }
        reject_link(&path)?;
        if path.is_dir() {
            walk(root, &path, output, visited)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(io_error)?
                .to_str()
                .ok_or_else(|| ResourceError::before("cache_corrupt", "缓存文件名编码无效"))?
                .replace('\\', "/");
            if relative != RECEIPT {
                if !safe_relative(&relative) || output.len() >= MAX_FILES {
                    return Err(ResourceError::before("cache_corrupt", "缓存目录含非法路径"));
                }
                output.insert(relative, hash_file(&path)?);
            }
        } else {
            return Err(ResourceError::before("cache_corrupt", "缓存目录含特殊文件"));
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    if fs::metadata(path).map_err(io_error)?.len() > MAX_EXPANDED_BYTES {
        return Err(ResourceError::before("cache_corrupt", "缓存文件大小超限"));
    }
    let mut input = File::open(path).map_err(io_error)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = input.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_EXPANDED_BYTES {
            return Err(ResourceError::before("cache_corrupt", "缓存文件读取超限"));
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn prepare_root(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(io_error)?;
    let mut current = Some(root);
    while let Some(path) = current {
        if path.as_os_str().is_empty() {
            break;
        }
        reject_link(path)?;
        current = path.parent();
    }
    Ok(())
}

fn reject_link(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt as _;
        link || metadata.file_attributes() & 0x400 != 0
    };
    if link {
        return Err(ResourceError::before(
            "unsafe_cache",
            "缓存路径包含链接或重解析点",
        ));
    }
    Ok(())
}

fn io_error<T>(_: T) -> ResourceError {
    ResourceError::before("cache_io", "资源缓存文件操作失败")
}

/// 校验 ZIP 的大小、路径、链接、压缩比、CRC 与解压限制，不执行包内容。
///
/// # Errors
/// 文件超过 1 GiB、ZIP 损坏或条目不安全时拒绝。
pub fn validate_archive(path: &Path) -> Result<()> {
    reject_link(path)?;
    let size = fs::metadata(path).map_err(io_error)?.len();
    if size == 0 || size > 1024 * 1024 * 1024 {
        return Err(ResourceError::before(
            "zip_limit",
            "ZIP 大小必须在 1 GiB 以内",
        ));
    }
    let target = tempfile::tempdir().map_err(io_error)?;
    extract(path, target.path(), "validation")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(name: &str, bytes: &[u8]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = zip::ZipWriter::new(File::create(file.path()).unwrap());
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap();
        file
    }

    #[test]
    fn extraction_rejects_escape_and_reserved_receipt() {
        for name in [
            "../outside",
            "a/../../outside",
            "C:/outside",
            "NUL",
            ".remoteops-integrity.json",
        ] {
            let archive = archive(name, b"x");
            let root = tempfile::tempdir().unwrap();
            assert!(
                extract(archive.path(), root.path(), "digest").is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn startup_integrity_detects_modification_and_extra_file() {
        let archive = archive("bin/tool.exe", b"test");
        let directory = tempfile::tempdir().unwrap();
        let root = tempfile::Builder::new()
            .tempdir_in(directory.path())
            .unwrap();
        let digest = hash_file(archive.path()).unwrap();
        fs::copy(archive.path(), root.path().with_extension("zip")).unwrap();
        extract(archive.path(), root.path(), &digest).unwrap();
        verify(root.path(), &digest).unwrap();
        fs::write(root.path().join("bin/tool.exe"), b"changed").unwrap();
        assert!(verify(root.path(), &digest).is_err());
        fs::write(root.path().join("bin/tool.exe"), b"test").unwrap();
        fs::write(root.path().join("extra"), b"x").unwrap();
        assert!(verify(root.path(), &digest).is_err());
    }

    #[test]
    fn zip_links_case_aliases_and_expansion_bombs_are_rejected() {
        for mode in 0..3 {
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut archive = zip::ZipWriter::new(File::create(file.path()).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            match mode {
                0 => {
                    archive.add_symlink("link", "../outside", options).unwrap();
                }
                1 => {
                    for name in ["File.txt", "file.txt"] {
                        archive.start_file(name, options).unwrap();
                        archive.write_all(b"x").unwrap();
                    }
                }
                _ => {
                    archive.start_file("large", options).unwrap();
                    archive.write_all(&vec![0; 100_000]).unwrap();
                }
            }
            archive.finish().unwrap();
            let target = tempfile::tempdir().unwrap();
            assert!(
                extract(file.path(), target.path(), "digest").is_err(),
                "ZIP 模式 {mode}"
            );
        }
    }
}
