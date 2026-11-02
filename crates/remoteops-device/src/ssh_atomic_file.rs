//! SSH 信任库唯一的原子替换平台边界，不使用搬走旧文件的两步提交。
use std::{
    os::windows::ffi::OsStrExt as _,
    path::Path,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Security::{
            DACL_SECURITY_INFORMATION, GetFileSecurityW, GetSecurityDescriptorControl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
            SetFileSecurityW, UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    },
    core::PCWSTR,
};

/// 将同目录新工具原子发布到尚不存在的路径，不替换并发赢家。
pub(super) fn publish_new(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // 不设置 REPLACE_EXISTING；rename 后不留下与最终锁定文件共享 inode 的临时别名。
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

fn copy_existing_dacl(source: PCWSTR, target: PCWSTR) -> Result<(), crate::DeviceError> {
    let failed = || crate::DeviceError::Operation("无法保留 SSH 信任库权限，未替换旧文件".into());
    let mut required = 0;
    // 先获取自相对安全描述符长度；不传入可写缓冲。
    let _ = unsafe {
        GetFileSecurityW(
            source,
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &raw mut required,
        )
    };
    if required == 0 || required > 65_536 {
        return Err(failed());
    }
    let mut buffer = vec![0_usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    let capacity =
        u32::try_from(buffer.len() * std::mem::size_of::<usize>()).map_err(|_| failed())?;
    let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
    // 对齐缓冲与长度有效，描述符直到 SetFileSecurityW 返回才释放。
    unsafe {
        GetFileSecurityW(
            source,
            DACL_SECURITY_INFORMATION.0,
            Some(descriptor),
            capacity,
            &raw mut required,
        )
    }
    .ok()
    .map_err(|_| failed())?;
    let mut control = 0;
    let mut revision = 0;
    unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) }
        .map_err(|_| failed())?;
    let inheritance = if control & SE_DACL_PROTECTED.0 != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    // 只复制 DACL 与其继承保护状态，不改 owner，也不要求 SACL 相关特权。
    unsafe { SetFileSecurityW(target, DACL_SECURITY_INFORMATION | inheritance, descriptor) }
        .ok()
        .map_err(|_| failed())
}

pub(super) fn replace(temporary: &Path, destination: &Path) -> Result<(), crate::DeviceError> {
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if destination
        .try_exists()
        .map_err(|_| crate::DeviceError::Operation("无法查询 SSH 信任库".into()))?
    {
        copy_existing_dacl(PCWSTR(target.as_ptr()), PCWSTR(source.as_ptr()))?;
    }
    let started = Instant::now();
    loop {
        // 同目录单次 rename 覆盖；失败保留旧路径。跨进程锁贯穿重读／合并／发布。
        // 缓冲均 NUL 结尾且贯穿调用，不使用 ReplaceFileW 的多步内部替换。
        let result = unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(target.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                let code = error.code().0 & 0xffff;
                // 只重试写侧短暂占用；读侧始终读取一次且不容忍任何缺口。
                if matches!(code, 5 | 32 | 33) && started.elapsed() < Duration::from_millis(500) {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                #[cfg(test)]
                eprintln!("trust_atomic_os_error={code}");
                return Err(crate::DeviceError::Operation(
                    "SSH 信任库原子替换失败，未报告信任成功".into(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::windows::process::CommandExt as _, path::PathBuf};
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> Directory {
        let path = std::env::temp_dir().join(format!(
            "remoteops-trust-acl-{}",
            remoteops_domain::RequestId::new()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Directory(path)
    }
    fn acl_check(path: &Path, initialize: bool) {
        let literal = path.to_string_lossy().replace('\'', "''");
        let prefix = if initialize {
            "$a=New-Object System.Security.AccessControl.FileSecurity; $a.SetAccessRuleProtection($true,$false); $s=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $r=New-Object System.Security.AccessControl.FileSystemAccessRule($s,'FullControl','Allow'); $a.AddAccessRule($r); [System.IO.File]::SetAccessControl($p,$a);"
        } else {
            ""
        };
        let script = format!(
            "$p='{literal}'; {prefix} $a=[System.IO.File]::GetAccessControl($p); if(-not $a.AreAccessRulesProtected){{exit 2}}; $r=@($a.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])); if($r.Count -ne 1 -or $r[0].IdentityReference.Value -ne [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value -or $r[0].FileSystemRights -ne [System.Security.AccessControl.FileSystemRights]::FullControl){{exit 3}}"
        );
        let executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = std::process::Command::new(executable)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(output.status.success(), "ACL 校验失败；未输出描述符或路径");
    }
    #[test]
    fn atomic_replace_preserves_owner_only_protected_dacl_and_failure_keeps_old_file() {
        let directory = fixture();
        let destination = directory.0.join("known_hosts");
        std::fs::write(&destination, b"old\n").unwrap();
        acl_check(&destination, true);
        let temporary = directory.0.join("replacement");
        std::fs::write(&temporary, b"new\n").unwrap();
        replace(&temporary, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new\n");
        acl_check(&destination, false);
        assert!(replace(&directory.0.join("missing"), &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"new\n");
        acl_check(&destination, false);
    }
}
