//! 无网络的原生 GUID 生成结果对话框。
use std::{io, ptr};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::{
        Controls::EM_SETSEL,
        WindowsAndMessaging::{
            DS_CENTER, DS_SETFONT, DialogBoxIndirectParamW, ES_AUTOHSCROLL, ES_READONLY, EndDialog,
            GetDlgItem, IDCANCEL, IDOK, MB_ICONERROR, MessageBoxW, SendMessageW, WM_CLOSE,
            WM_COMMAND, WM_COPY, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
            WS_VISIBLE,
        },
    },
};

const KEY_FIELD_ID: u16 = 101;
const COPY_BUTTON_ID: u16 = 102;
const BUTTON_CLASS: u16 = 0x0080;
const EDIT_CLASS: u16 = 0x0081;

fn push_text(words: &mut Vec<u16>, text: &str) {
    words.extend(text.encode_utf16().chain(Some(0)));
}

fn push_dword(words: &mut Vec<u16>, value: u32) {
    let [a, b, c, d] = value.to_le_bytes();
    words.extend([u16::from_le_bytes([a, b]), u16::from_le_bytes([c, d])]);
}

fn add_control(
    words: &mut Vec<u16>,
    style: u32,
    geometry: [u16; 4],
    id: u16,
    class: u16,
    text: &str,
) {
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
    push_dword(words, style | WS_CHILD | WS_VISIBLE | WS_TABSTOP);
    push_dword(words, 0);
    words.extend(geometry);
    words.extend([id, 0xffff, class]);
    push_text(words, text);
    words.push(0);
}

/// 显示可选择和复制 GUID 的本机对话框，窗口关闭后返回。
///
/// # Errors
///
/// Windows 无法创建对话框时返回系统错误。
#[allow(unsafe_code)]
pub fn show_test_key_dialog(guid: &str) -> io::Result<()> {
    let mut words = Vec::new();
    push_dword(
        &mut words,
        WS_POPUP
            | WS_CAPTION
            | WS_SYSMENU
            | u32::try_from(DS_CENTER | DS_SETFONT).unwrap_or_default(),
    );
    push_dword(&mut words, 0);
    words.extend([3, 0, 0, 210, 58, 0, 0]);
    push_text(&mut words, "RemoteOps 测试 GUID");
    words.push(9);
    push_text(&mut words, "Segoe UI");
    add_control(
        &mut words,
        WS_BORDER | u32::try_from(ES_READONLY | ES_AUTOHSCROLL).unwrap_or_default(),
        [10, 10, 190, 14],
        KEY_FIELD_ID,
        EDIT_CLASS,
        guid,
    );
    add_control(
        &mut words,
        0,
        [90, 33, 52, 14],
        COPY_BUTTON_ID,
        BUTTON_CLASS,
        "复制",
    );
    add_control(
        &mut words,
        0,
        [148, 33, 52, 14],
        u16::try_from(IDCANCEL).unwrap_or_default(),
        BUTTON_CLASS,
        "关闭",
    );
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
    // 对话框模板要求按 DWORD 对齐，系统同步借用模板直到窗口关闭。
    let template = words
        .as_chunks::<2>()
        .0
        .iter()
        .map(|word| u32::from(word[0]) | (u32::from(word[1]) << 16))
        .collect::<Vec<_>>();
    if unsafe {
        DialogBoxIndirectParamW(
            ptr::null_mut(),
            template.as_ptr().cast(),
            ptr::null_mut(),
            Some(dialog_proc),
            0,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 在 GUI 启动失败时显示已脱敏的简短错误。
#[allow(unsafe_code)]
pub fn show_startup_error(message: &str) {
    let title = "RemoteOps"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let message = message.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    // 系统仅在同步调用期间读取两个以 NUL 结尾的字符串。
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR,
        )
    };
}

#[allow(unsafe_code)]
unsafe extern "system" fn dialog_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    _: LPARAM,
) -> isize {
    let command = i32::try_from(wparam & 0xffff).unwrap_or_default();
    if message == WM_CLOSE || (message == WM_COMMAND && (command == IDCANCEL || command == IDOK)) {
        unsafe { EndDialog(window, 0) };
        return 1;
    }
    if message == WM_COMMAND && command == i32::from(COPY_BUTTON_ID) {
        // 使用只读编辑框的原生复制操作，剪贴板里只有显示的 GUID。
        let field = unsafe { GetDlgItem(window, i32::from(KEY_FIELD_ID)) };
        unsafe {
            SendMessageW(field, EM_SETSEL, 0, -1);
            SendMessageW(field, WM_COPY, 0, 0);
        }
        return 1;
    }
    0
}
