//! Windows 控制台接入；不接触图形设备或改变已有重定向。

/// 确保当前进程可使用控制台，并保留已有的标准输入输出重定向。
///
/// Windows 优先附加父进程控制台，父进程没有控制台时才创建；其他平台不执行操作。
/// 应在初始化日志、标准流读取或启动后台线程之前调用。
///
/// # Errors
///
/// 无法附加或创建 Windows 控制台，或无法恢复已有标准流句柄时返回系统错误。
pub fn ensure_console() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows::ensure_console()
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use windows_sys::Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, GetHandleInformation, HANDLE,
            INVALID_HANDLE_VALUE,
        },
        System::Console::{
            ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
            STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        },
    };

    // 安全边界：只借用句柄并查询其有效性，系统只向本地 flags 变量写入。
    #[allow(unsafe_code)]
    fn valid_handle(handle: HANDLE) -> bool {
        let mut flags = 0;
        !handle.is_null()
            && handle != INVALID_HANDLE_VALUE
            && unsafe { GetHandleInformation(handle, &raw mut flags) } != 0
    }

    // 安全边界：启动期独占标准句柄表；只借用已有句柄，不关闭或转移其所有权。
    // AttachConsole/AllocConsole 可能替换标准句柄，随后恢复有效的文件和管道重定向。
    #[allow(unsafe_code)]
    pub(super) fn ensure_console() -> io::Result<()> {
        let handles = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|kind| {
            let handle = unsafe { GetStdHandle(kind) };
            (kind, valid_handle(handle).then_some(handle))
        });
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            let error = io::Error::last_os_error();
            match error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok())
            {
                // 已有关联控制台时保持原控制台和标准流，重复调用不重建窗口。
                Some(ERROR_ACCESS_DENIED) => return Ok(()),
                Some(ERROR_INVALID_HANDLE) => {
                    if unsafe { AllocConsole() } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                _ => return Err(error),
            }
        }
        let mut failure = None;
        for (kind, handle) in handles {
            if let Some(handle) = handle
                && unsafe { SetStdHandle(kind, handle) } == 0
                && failure.is_none()
            {
                failure = Some(io::Error::last_os_error());
            }
        }
        failure.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::io::AsRawHandle;

        #[test]
        fn redirected_file_handles_are_valid_but_absent_handles_are_not() {
            let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
            assert!(valid_handle(file.as_raw_handle()));
            assert!(!valid_handle(std::ptr::null_mut()));
            assert!(!valid_handle(INVALID_HANDLE_VALUE));
        }

        #[test]
        #[ignore = "在交互式 Windows 控制台执行，可能创建控制台窗口"]
        // 安全边界：只在显式运行的独立原生验收中读取标准流句柄，不关闭句柄。
        #[allow(unsafe_code)]
        fn console_initialization_preserves_redirected_handles_and_is_repeatable() {
            let before = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|kind| {
                let handle = unsafe { GetStdHandle(kind) };
                (kind, valid_handle(handle).then_some(handle))
            });
            super::ensure_console().unwrap();
            super::ensure_console().unwrap();
            for (kind, expected) in before {
                if let Some(expected) = expected {
                    assert_eq!(unsafe { GetStdHandle(kind) }, expected);
                }
            }
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn other_platforms_need_no_console_initialization() {
        super::ensure_console().unwrap();
    }
}
