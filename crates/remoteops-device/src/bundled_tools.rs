//! 单 EXE 中可信工具的延迟释放；不读取远端配置，也不改变 SSH 信任策略。
use crate::{DeviceError, metadata_is_reparse_point};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

/// 产品编译时嵌入的工具内容及审核后的 SHA-256。
#[derive(Clone, Copy)]
pub struct EmbeddedTool {
    /// 工具的完整原始字节，不是待执行命令或下载地址。
    pub bytes: &'static [u8],
    /// 与原始字节对应的 64 位十六进制 SHA-256。
    pub sha256: &'static str,
}
impl fmt::Debug for EmbeddedTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddedTool")
            .field("bytes", &self.bytes.len())
            .field("sha256", &self.sha256)
            .finish()
    }
}

/// 由本机产品宿主提供的不可变工具包，普通配置和远程请求不能修改。
#[derive(Clone, Debug)]
pub struct BundledTools {
    // 固定产品缓存根下的内容寻址目录，不能由工具文件名逃逸。
    directory: PathBuf,
    plink: EmbeddedTool,
    askpass: EmbeddedTool,
}

/// 已释放并核对摘要的路径；诊断调用不执行这些文件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundledToolPaths {
    /// 实际 Plink 路径。
    pub plink: PathBuf,
    /// 实际 askpass helper 路径。
    pub askpass: PathBuf,
}

// 实际执行期间保持文件及祖先目录句柄；Windows 禁止并发改写、删除或替换。
pub(crate) struct PreparedTool {
    path: PathBuf,
    _file: File,
    _directories: Vec<File>,
}
impl PreparedTool {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn invalid(message: &str) -> DeviceError {
    DeviceError::InvalidInput(message.into())
}
fn operation(error: &std::io::Error) -> DeviceError {
    DeviceError::Operation(format!("内置工具缓存访问失败：{error}"))
}

impl BundledTools {
    /// 创建内容寻址工具包；此步骤不释放工具，也不访问缓存目录。
    ///
    /// # Errors
    /// 缓存根不是绝对本地路径，或资源摘要格式无效时返回错误。
    pub fn new(
        mut cache_root: PathBuf,
        plink: EmbeddedTool,
        askpass: EmbeddedTool,
    ) -> Result<Self, DeviceError> {
        if !cache_root.is_absolute()
            || cache_root
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(invalid("内置工具缓存根必须是固定绝对路径"));
        }
        #[cfg(windows)]
        if !matches!(
            cache_root.components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))
        ) {
            return Err(invalid("内置工具缓存不能使用网络或设备路径"));
        }
        for tool in [plink, askpass] {
            if tool.bytes.is_empty()
                || tool.sha256.len() != 64
                || !tool.sha256.bytes().all(|value| value.is_ascii_hexdigit())
            {
                return Err(invalid("内置工具内容或 SHA-256 声明无效"));
            }
        }
        let identifier = format!(
            "{:x}",
            Sha256::digest(format!(
                "plink={}\naskpass={}",
                plink.sha256.to_ascii_lowercase(),
                askpass.sha256.to_ascii_lowercase()
            ))
        );
        cache_root.push(identifier);
        Ok(Self {
            directory: cache_root,
            plink,
            askpass,
        })
    }

    /// 返回不会随工作目录变化的真实缓存目录。
    #[must_use]
    pub fn cache_directory(&self) -> &Path {
        &self.directory
    }

    /// 释放并校验全部资源，供无窗口完整性诊断使用，不执行工具。
    ///
    /// # Errors
    /// 资源摘要不符、路径含链接、缓存已损坏或无法安全发布时返回错误。
    pub fn prepare_all(&self) -> Result<BundledToolPaths, DeviceError> {
        let plink = self.prepare_plink()?;
        let askpass = self.prepare_askpass()?;
        Ok(BundledToolPaths {
            plink: plink.path.clone(),
            askpass: askpass.path.clone(),
        })
    }

    pub(crate) fn prepare_plink(&self) -> Result<PreparedTool, DeviceError> {
        self.prepare("plink.exe", self.plink)
    }

    pub(crate) fn prepare_askpass(&self) -> Result<PreparedTool, DeviceError> {
        self.prepare("remoteops-ssh-askpass.exe", self.askpass)
    }

    fn prepare(&self, name: &str, tool: EmbeddedTool) -> Result<PreparedTool, DeviceError> {
        if !format!("{:x}", Sha256::digest(tool.bytes)).eq_ignore_ascii_case(tool.sha256) {
            return Err(invalid("内置工具资源 SHA-256 不匹配，未释放或执行"));
        }
        let directories = create_locked_directories(&self.directory)?;
        let path = self.directory.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return verified(path, tool, directories),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(operation(&error)),
        }
        let temporary = self.directory.join(format!(
            ".{name}.{}-{}.part",
            std::process::id(),
            remoteops_domain::RequestId::new()
        ));
        let file = private_new_file(&temporary)?;
        let mut temporary_guard = TemporaryTool {
            path: temporary.clone(),
            file: Some(file),
        };
        let file = temporary_guard
            .file
            .as_mut()
            .expect("新文件仍由本次释放持有");
        file.write_all(tool.bytes)
            .map_err(|error| operation(&error))?;
        file.sync_all().map_err(|error| operation(&error))?;
        drop(temporary_guard.file.take());
        // 不覆盖既有路径的原子发布；并发赢家仍须重新核验。
        match publish_new(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(operation(&error)),
        }
        drop(temporary_guard);
        verified(path, tool, directories)
    }
}

struct TemporaryTool {
    path: PathBuf,
    file: Option<File>,
}
impl Drop for TemporaryTool {
    fn drop(&mut self) {
        drop(self.file.take());
        #[cfg(windows)]
        {
            let started = std::time::Instant::now();
            loop {
                match std::fs::remove_file(&self.path) {
                    Err(error)
                        if matches!(error.raw_os_error(), Some(32 | 33))
                            && started.elapsed() < std::time::Duration::from_millis(500) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Ok(()) | Err(_) => break,
                }
            }
        }
        #[cfg(not(windows))]
        let _ = std::fs::remove_file(&self.path);
    }
}

fn publish_new(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    return crate::ssh_atomic_file::publish_new(temporary, destination);
    #[cfg(not(windows))]
    std::fs::hard_link(temporary, destination)
}

fn private_new_file(path: &Path) -> Result<File, DeviceError> {
    #[cfg(windows)]
    {
        let sid = crate::ssh_password_file::current_sid()?;
        crate::ssh_password_file::create_private_file(path, &sid)
            .map_err(|_| invalid("无法创建当前用户专属的内置工具文件"))
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        crate::apply_no_follow_flag(&mut options);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map_err(|error| operation(&error))
    }
}

fn reject_link(metadata: &std::fs::Metadata) -> Result<(), DeviceError> {
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata) {
        return Err(invalid("内置工具缓存路径含链接或重解析点，拒绝使用"));
    }
    Ok(())
}

fn create_locked_directories(path: &Path) -> Result<Vec<File>, DeviceError> {
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if !matches!(component, Component::Normal(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => reject_link(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    builder
                };
                match builder.create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(operation(&error)),
                }
            }
            Err(error) => return Err(operation(&error)),
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // 不跟随重解析点，且持有期间不允许删除/改名祖先目录。
            options
                .custom_flags(0x0200_0000 | 0x0020_0000)
                .share_mode(3);
        }
        #[cfg(not(windows))]
        crate::apply_no_follow_flag(&mut options);
        let file = options.open(&current).map_err(|error| operation(&error))?;
        let metadata = file.metadata().map_err(|error| operation(&error))?;
        reject_link(&metadata)?;
        if !metadata.is_dir() {
            return Err(invalid("内置工具缓存的祖先不是目录"));
        }
        handles.push(file);
    }
    Ok(handles)
}

fn verified(
    path: PathBuf,
    tool: EmbeddedTool,
    directories: Vec<File>,
) -> Result<PreparedTool, DeviceError> {
    reject_link(&std::fs::symlink_metadata(&path).map_err(|error| operation(&error))?)?;
    let mut options = OpenOptions::new();
    options.read(true);
    crate::apply_no_follow_flag(&mut options);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut file = options.open(&path).map_err(|error| operation(&error))?;
    let metadata = file.metadata().map_err(|error| operation(&error))?;
    reject_link(&metadata)?;
    if !metadata.is_file() || metadata.len() != tool.bytes.len() as u64 {
        return Err(invalid("内置工具缓存大小不符，拒绝执行"));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let length = file.read(&mut buffer).map_err(|error| operation(&error))?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(tool.sha256) {
        return Err(invalid("内置工具缓存 SHA-256 不符，拒绝执行"));
    }
    Ok(PreparedTool {
        path,
        _file: file,
        _directories: directories,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "remoteops-bundled-tools-{}",
                remoteops_domain::RequestId::new()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            // 只清理本测试创建、仍位于预期临时根内且不是链接的目录。
            let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            if let Ok(path) = std::fs::canonicalize(&self.0)
                && path.parent() == Some(parent.as_path())
                && path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("remoteops-bundled-tools-")
                && std::fs::symlink_metadata(&self.0).is_ok_and(|metadata| {
                    !metadata_is_reparse_point(&metadata) && !metadata.file_type().is_symlink()
                })
            {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }

    fn asset(bytes: &'static [u8]) -> EmbeddedTool {
        EmbeddedTool {
            bytes,
            sha256: Box::leak(format!("{:x}", Sha256::digest(bytes)).into_boxed_str()),
        }
    }
    fn bundle(root: &Path) -> BundledTools {
        BundledTools::new(
            root.join("runtime"),
            asset(b"plink-fixture"),
            asset(b"askpass-fixture"),
        )
        .unwrap()
    }

    #[test]
    fn constructor_is_lazy_and_requires_fixed_absolute_cache_and_valid_hash_declarations() {
        let root = Directory::new();
        let tools = bundle(&root.0);
        let device = crate::SystemDevice::new().with_bundled_tools(Some(tools.clone()));
        assert!(device.has_bundled_ssh());
        assert!(!crate::SystemDevice::new().has_bundled_ssh());
        assert!(!tools.cache_directory().exists());
        assert!(BundledTools::new("relative".into(), asset(b"a"), asset(b"b")).is_err());
        assert!(BundledTools::new(root.0.join("a/../b"), asset(b"a"), asset(b"b")).is_err());
        assert!(
            BundledTools::new(
                root.0.clone(),
                EmbeddedTool {
                    bytes: b"a",
                    sha256: "x"
                },
                asset(b"b")
            )
            .is_err()
        );
    }

    #[test]
    fn prepare_all_publishes_exact_assets_and_reuses_content_addressed_paths() {
        let root = Directory::new();
        let tools = bundle(&root.0);
        let first = tools.prepare_all().unwrap();
        assert_eq!(std::fs::read(&first.plink).unwrap(), b"plink-fixture");
        assert_eq!(std::fs::read(&first.askpass).unwrap(), b"askpass-fixture");
        assert_eq!(tools.prepare_all().unwrap(), first);
        assert_eq!(
            std::fs::read_dir(tools.cache_directory()).unwrap().count(),
            2
        );
    }

    #[test]
    fn preparing_one_backend_does_not_release_other_tool() {
        let root = Directory::new();
        let tools = bundle(&root.0);
        let plink = tools.prepare_plink().unwrap();
        assert!(plink.path().is_file());
        assert!(
            !tools
                .cache_directory()
                .join("remoteops-ssh-askpass.exe")
                .exists()
        );
    }

    #[test]
    fn damaged_embedded_asset_fails_before_creating_any_cache() {
        let root = Directory::new();
        let good = asset(b"original");
        let tools = BundledTools::new(
            root.0.join("runtime"),
            EmbeddedTool {
                bytes: b"changed",
                ..good
            },
            asset(b"b"),
        )
        .unwrap();
        assert!(tools.prepare_all().is_err());
        assert!(!tools.cache_directory().exists());
    }

    #[test]
    fn corrupt_cache_is_rejected_without_silent_fallback_or_overwrite() {
        let root = Directory::new();
        let tools = bundle(&root.0);
        let paths = tools.prepare_all().unwrap();
        std::fs::write(&paths.plink, b"untrusted").unwrap();
        assert!(tools.prepare_all().is_err());
        assert_eq!(std::fs::read(&paths.plink).unwrap(), b"untrusted");
    }

    #[test]
    fn simultaneous_instances_publish_once_and_leave_no_partial_files() {
        let root = Directory::new();
        for round in 0..8 {
            let tools = Arc::new(bundle(&root.0.join(format!("round-{round}"))));
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let tools = Arc::clone(&tools);
                    std::thread::spawn(move || tools.prepare_all().unwrap())
                })
                .collect();
            let expected = tools.prepare_all().unwrap();
            for thread in threads {
                assert_eq!(thread.join().unwrap(), expected);
            }
            assert_eq!(
                std::fs::read_dir(tools.cache_directory()).unwrap().count(),
                2
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn held_verified_handle_prevents_cache_rewrite_and_delete() {
        let root = Directory::new();
        let tools = bundle(&root.0);
        let guard = tools.prepare_plink().unwrap();
        let path = guard.path().to_path_buf();
        assert!(std::fs::write(&path, b"tampered").is_err());
        assert!(std::fs::remove_file(&path).is_err());
        drop(guard);
        std::fs::write(&path, b"tampered").unwrap();
        assert!(tools.prepare_plink().is_err());
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn reparse_or_symlink_ancestor_is_rejected_without_writing_target() {
        let root = Directory::new();
        let target = root.0.join("target");
        let link = root.0.join("runtime");
        std::fs::create_dir(&target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // 只创建测试自有目录的 junction；无凭据、无网络，也不显示控制台。
            let status = std::process::Command::new("cmd.exe")
                .args(["/D", "/S", "/C"])
                .raw_arg(format!(
                    "\"mklink /J \"{}\" \"{}\"\"",
                    link.display(),
                    target.display()
                ))
                .creation_flags(0x0800_0000)
                .output()
                .unwrap();
            assert!(status.status.success(), "自有测试 junction 创建失败");
        }
        let tools = bundle(&root.0);
        assert!(tools.prepare_all().is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
    }
}
