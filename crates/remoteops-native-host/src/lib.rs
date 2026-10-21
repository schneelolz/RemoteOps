//! 最小宿主事实与控制台封装；不加载图形 Provider、不枚举窗口、不截图、不注入输入。

mod console;

pub use console::ensure_console;

/// 当前进程交互桌面的系统证据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopFacts {
    /// 当前进程所属 Windows Session；查询失败或非 Windows 时未知。
    pub process_session_id: Option<u32>,
    /// 当前进程可以访问普通交互输入桌面；查询失败时未知。
    pub interactive_desktop: Option<bool>,
    /// 稳定的事实来源或不可用原因。
    pub reason: &'static str,
}

/// 读取当前进程所属会话与桌面，只查询本机对象元数据。
#[must_use]
pub fn collect() -> DesktopFacts {
    #[cfg(windows)]
    {
        windows::collect()
    }
    #[cfg(not(windows))]
    {
        DesktopFacts {
            process_session_id: None,
            interactive_desktop: None,
            reason: "unsupported_platform",
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::DesktopFacts;
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetThreadDesktop,
                GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
            },
            Threading::GetCurrentThreadId,
        },
    };

    // 安全边界：仅借用系统返回的有效对象句柄，缓冲区固定分配且长度按字节传递。
    #[allow(unsafe_code)]
    fn object_name(handle: HANDLE) -> Option<String> {
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 512];
        let mut needed = 0;
        let success = unsafe {
            GetUserObjectInformationW(
                handle,
                UOI_NAME,
                buffer.as_mut_ptr().cast(),
                u32::try_from(std::mem::size_of_val(&buffer)).ok()?,
                &raw mut needed,
            )
        };
        if success == 0 {
            return None;
        }
        let length = buffer.iter().position(|unit| *unit == 0)?;
        String::from_utf16(&buffer[..length]).ok()
    }

    // 所有句柄来源于当前进程/线程。仅关闭本函数打开的输入桌面，不关闭借用句柄。
    #[allow(unsafe_code)]
    pub(super) fn collect() -> DesktopFacts {
        let mut session = 0;
        if unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session) } == 0 {
            return DesktopFacts {
                process_session_id: None,
                interactive_desktop: None,
                reason: "process_session_query_failed",
            };
        }
        let facts = |available, reason| DesktopFacts {
            process_session_id: Some(session),
            interactive_desktop: available,
            reason,
        };
        if session == 0 {
            return facts(Some(false), "session_zero");
        }
        let station = object_name(unsafe { GetProcessWindowStation() });
        let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) });
        let (Some(station), Some(desktop)) = (station, desktop) else {
            return facts(None, "desktop_metadata_query_failed");
        };
        if !station.eq_ignore_ascii_case("WinSta0") || !desktop.eq_ignore_ascii_case("Default") {
            return facts(Some(false), "noninteractive_desktop");
        }
        let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
        if input.is_null() {
            return facts(None, "input_desktop_unavailable");
        }
        let input_name = object_name(input);
        // OpenInputDesktop 获得的句柄归本函数所有，所有后续路径都在此释放。
        unsafe {
            CloseDesktop(input);
        }
        match input_name {
            Some(name) if name.eq_ignore_ascii_case("Default") => {
                facts(Some(true), "native_windows_api")
            }
            Some(_) => facts(Some(false), "secure_input_desktop"),
            None => facts(None, "input_desktop_metadata_failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_facts_are_consistent_without_claiming_service_validation() {
        let facts = super::collect();
        assert_ne!(facts.reason, "");
        if facts.process_session_id == Some(0) {
            assert_eq!(facts.interactive_desktop, Some(false));
        }
    }
}
