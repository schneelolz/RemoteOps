//! 唯一的系统 ACL 边界：文件创建时即仅允许当前身份访问，口令不进入命令行或环境。

use super::DeviceError;
#[cfg(windows)]
use std::os::windows::io::FromRawHandle;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
#[cfg(windows)]
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_TEMPORARY, FILE_SHARE_READ},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
    core::{PCWSTR, PWSTR},
};

pub(super) struct PasswordFile {
    path: PathBuf,
}
impl PasswordFile {
    pub(super) fn new(password: &str) -> Result<Self, DeviceError> {
        if password.contains(['\r', '\n', '\0']) {
            return Err(DeviceError::InvalidInput(
                "SSH 密码不能包含换行或 NUL".to_owned(),
            ));
        }
        #[cfg(windows)]
        let sid = current_sid()?;
        #[cfg(not(windows))]
        let sid = String::new();
        let path = std::env::temp_dir().join(format!(
            "remoteops-ssh-{}.pw",
            remoteops_domain::RequestId::new()
        ));
        let mut file = create_private_file(&path, &sid)?;
        let guard = Self { path };
        let written = file
            .write_all(password.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        written.map_err(|_| DeviceError::Operation("无法写入 SSH 临时凭据".to_owned()))?;
        Ok(guard)
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for PasswordFile {
    fn drop(&mut self) {
        #[cfg(windows)]
        remove_with_sharing_retry(&self.path);
        #[cfg(not(windows))]
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn remove_with_sharing_retry(path: &Path) {
    use std::time::{Duration, Instant};
    let started = Instant::now();
    let budget = Duration::from_millis(500);
    loop {
        match std::fs::remove_file(path) {
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {
                // Job 终止与文件句柄关闭可能稍晚，只为共享/锁冲突保留短暂清理窗口。
                let remaining = budget.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return;
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Ok(()) | Err(_) => return,
        }
    }
}

#[cfg(windows)]
pub(super) fn current_sid() -> Result<String, DeviceError> {
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // 仅关闭 OpenProcessToken 成功返回的自有句柄，不关闭进程伪句柄。
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let failed = || DeviceError::Operation("无法核对 SSH 凭据所属身份".to_owned());
    let mut handle = HANDLE::default();
    // 当前进程伪句柄仅用于查询；返回 token 由局部 RAII 关闭。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut handle) }
        .map_err(|_| failed())?;
    let token = Token(handle);
    let mut required = 0;
    // 首次只获取长度；系统预期返回缓冲不足，不读取空指针。
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &raw mut required) };
    if required == 0 || required > 65_536 {
        return Err(failed());
    }
    // usize 保证 TOKEN_USER 的指针对齐，缓冲在 SID 转换结束前保持存活。
    let mut buffer = vec![0_usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    let capacity =
        u32::try_from(buffer.len() * std::mem::size_of::<usize>()).map_err(|_| failed())?;
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            capacity,
            &raw mut required,
        )
    }
    .map_err(|_| failed())?;
    if (required as usize) < std::mem::size_of::<TOKEN_USER>() || required > capacity {
        return Err(failed());
    }
    // 成功的 TokenUser 查询保证结构及其内部 SID 位于仍存活的系统输出缓冲。
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut sid) }.map_err(|_| failed())?;
    // API 保证成功时返回 NUL 结尾 UTF-16；所有返回路径释放 LocalAlloc 内存。
    let text = unsafe { sid.to_string() };
    unsafe { LocalFree(Some(HLOCAL(sid.0.cast()))) };
    text.map_err(|_| failed())
}

#[cfg(windows)]
pub(super) fn create_private_file(path: &Path, sid: &str) -> Result<std::fs::File, DeviceError> {
    use std::os::windows::ffi::OsStrExt as _;
    let sddl: Vec<_> = format!("D:P(A;;FA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let name: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SDDL 缓冲以 NUL 终止且贯穿调用；返回描述符由 LocalFree 释放。
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &raw mut descriptor,
            None,
        )
    }
    .map_err(|_| DeviceError::Operation("无法创建 SSH 凭据访问控制".to_owned()))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .expect("系统结构大小为DWORD"),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    // CREATE_NEW 拒绝已有文件；DACL 在创建操作中生效，句柄不继承给子进程。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(&raw const attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_TEMPORARY,
            None,
        )
    };
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    let handle =
        handle.map_err(|_| DeviceError::Operation("无法安全创建 SSH 临时凭据".to_owned()))?;
    // 成功的独占所有权句柄移交 File，之后只由 File 关闭一次。
    Ok(unsafe { std::fs::File::from_raw_handle(handle.0) })
}

#[cfg(unix)]
fn create_private_file(path: &Path, _sid: &str) -> Result<std::fs::File, DeviceError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| DeviceError::Operation("无法安全创建 SSH 临时凭据".to_owned()))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt as _;

    fn assert_owner_only(path: &Path) {
        let mut script = format!(
            "$a=[System.IO.File]::GetAccessControl('{}'); ",
            path.display().to_string().replace('\'', "''")
        );
        script.push_str("if(-not $a.AreAccessRulesProtected){exit 2}; $r=@($a.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])); if($r.Count -ne 1 -or $r[0].IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value -ne [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value){exit 3}");
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .status()
            .unwrap();
        assert!(status.success());
    }

    struct TestCleanup(PathBuf);
    impl Drop for TestCleanup {
        fn drop(&mut self) {
            remove_with_sharing_retry(&self.0);
        }
    }

    #[test]
    fn credential_is_owner_only_and_removed_on_drop() {
        let value = PasswordFile::new("test-secret-not-a-real-password").unwrap();
        let path = value.path().to_owned();
        assert!(path.is_file());
        assert_owner_only(&path);
        drop(value);
        assert!(!path.exists());
    }

    #[test]
    fn drop_retries_until_a_temporary_file_lock_is_released() {
        use std::{
            os::windows::fs::OpenOptionsExt as _,
            time::{Duration, Instant},
        };
        let value = PasswordFile::new("fake-test-secret").unwrap();
        let path = value.path().to_owned();
        let _cleanup = TestCleanup(path.clone());
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let started = Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(lock);
        });
        drop(value);
        let elapsed = started.elapsed();
        release.join().unwrap();
        assert!(elapsed >= Duration::from_millis(150));
        assert!(elapsed < Duration::from_millis(1500));
        assert!(!path.exists());
    }

    #[test]
    fn drop_stops_after_budget_when_file_remains_locked() {
        use std::{
            os::windows::fs::OpenOptionsExt as _,
            time::{Duration, Instant},
        };
        let value = PasswordFile::new("fake-test-secret").unwrap();
        let path = value.path().to_owned();
        let _cleanup = TestCleanup(path.clone());
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let (done, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let started = Instant::now();
            drop(value);
            done.send(started.elapsed()).unwrap();
        });
        let elapsed = result
            .recv_timeout(Duration::from_secs(2))
            .expect("持续共享冲突不能无限阻塞 Drop");
        assert!(elapsed >= Duration::from_millis(500));
        assert!(elapsed < Duration::from_millis(1500));
        assert!(path.exists());
        worker.join().unwrap();
        drop(lock);
        assert_owner_only(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn existing_files_cannot_be_overwritten_and_invalid_password_is_rejected() {
        assert!(PasswordFile::new("bad\npassword").is_err());
        let value = PasswordFile::new("test").unwrap();
        assert!(create_private_file(value.path(), &current_sid().unwrap()).is_err());
        assert_eq!(std::fs::read(value.path()).unwrap(), b"test");
    }
}
