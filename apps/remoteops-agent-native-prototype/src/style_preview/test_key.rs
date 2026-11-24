//! 与主窗口共用 Win32 配色、字体、按钮和剪贴板工作线程的离线对话框。
use super::{
    App, Cell, ES_AUTOHSCROLL, ES_READONLY, GetFocus, GetKeyState, HDC, MSG, Palette, Panel, RECT,
    RefCell, State, Theme, VK_CONTROL, VK_RETURN, WM_KEYDOWN, WS_TABSTOP, add, button, clipboard,
    close_panel, label, round,
};
use remoteops_agent_desktop_host::{NativeAppearance, NativeLanguage, UiFlavor};
use remoteops_product::ProductProfile;

const GUID: i32 = 101;
pub(super) const COPY: i32 = 102;
const CLOSE: i32 = 106;
const STATUS: i32 = 105;

#[derive(Clone, Copy, Default)]
enum Feedback {
    #[default]
    Ready,
    Pending,
    Copied,
    Failed,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Preferences {
    language: NativeLanguage,
    appearance: NativeAppearance,
}

pub(super) struct Dialog {
    guid: String,
    product_name: String,
    preferences: Preferences,
    clipboard: RefCell<clipboard::Clipboard>,
    feedback: Cell<Feedback>,
}

impl Dialog {
    pub(super) fn new(guid: String, profile: &ProductProfile) -> Self {
        let path = profile
            .paths()
            .gui_preferences
            .with_file_name(UiFlavor::Win32.preferences_filename());
        let preferences = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            guid,
            product_name: profile.name.clone(),
            preferences,
            clipboard: RefCell::default(),
            feedback: Cell::default(),
        }
    }

    pub(super) fn state(&self) -> State {
        State {
            english: self.preferences.language == NativeLanguage::English,
            theme: match self.preferences.appearance {
                NativeAppearance::System => Theme::System,
                NativeAppearance::Light => Theme::Light,
                NativeAppearance::Dark => Theme::Dark,
            },
            ..State::default()
        }
    }

    fn feedback_text(&self, app: &App) -> (&'static str, &'static str) {
        match self.feedback.get() {
            Feedback::Ready => (app.t("复制", "Copy"), ""),
            Feedback::Pending => (app.t("正在复制", "Copying"), ""),
            Feedback::Copied => (app.t("已复制", "Copied"), ""),
            Feedback::Failed => (
                app.t("复制", "Copy"),
                app.t(
                    "复制失败，可选择 GUID 手动复制",
                    "Copy failed. Select the GUID to copy it.",
                ),
            ),
        }
    }
}

fn dialog(panel: &Panel) -> &Dialog {
    panel.app().test_key.as_ref().expect("已初始化的 GUID 窗口")
}

pub(super) fn title(app: &App) -> String {
    let dialog = app.test_key.as_ref().expect("已初始化的 GUID 窗口");
    format!(
        "{} · {}",
        dialog.product_name,
        app.t("生成测试 GUID", "Generate test GUID")
    )
}

pub(super) fn build(panel: &Panel) {
    let app = panel.app();
    let dialog = dialog(panel);
    label(
        panel,
        100,
        app.t("测试 GUID", "Test GUID"),
        18,
        15,
        412,
        26,
        1,
        false,
    );
    add(
        panel,
        GUID,
        "EDIT",
        &dialog.guid,
        30,
        63,
        388,
        23,
        WS_TABSTOP | (ES_AUTOHSCROLL | ES_READONLY) as u32,
        4,
    );
    let (copy, status) = dialog.feedback_text(app);
    label(panel, STATUS, status, 18, 121, 208, 38, 3, false);
    button(panel, CLOSE, app.t("关闭", "Close"), 232, 118, 86, 36);
    button(panel, COPY, copy, 330, 118, 100, 36);
}

pub(super) fn paint(panel: &Panel, hdc: HDC, colors: Palette) {
    round(
        hdc,
        RECT {
            left: panel.px(18),
            top: panel.px(51),
            right: panel.px(430),
            bottom: panel.px(96),
        },
        colors.content,
        colors.border,
        panel.px(8),
    );
}

pub(super) fn command(panel: &Panel, id: i32) {
    match id {
        CLOSE => close_panel(panel),
        COPY => {
            let dialog = dialog(panel);
            let result = dialog.clipboard.borrow_mut().request(&dialog.guid);
            set_feedback(
                panel,
                if result.is_ok() {
                    Feedback::Pending
                } else {
                    Feedback::Failed
                },
            );
        }
        _ => {}
    }
}

pub(super) fn poll(panel: &Panel) {
    let result = dialog(panel).clipboard.borrow_mut().poll();
    if let Some(result) = result {
        set_feedback(
            panel,
            match result {
                clipboard::Outcome::Copied => Feedback::Copied,
                clipboard::Outcome::Failed => Feedback::Failed,
            },
        );
    }
}

fn set_feedback(panel: &Panel, feedback: Feedback) {
    let dialog = dialog(panel);
    dialog.feedback.set(feedback);
    let (caption, status) = dialog.feedback_text(panel.app());
    panel.text(COPY, caption);
    panel.text(STATUS, status);
}

pub(super) fn handle_key(panel: &Panel, message: &MSG) -> bool {
    if message.message != WM_KEYDOWN {
        return false;
    }
    let focus = unsafe { GetFocus() };
    if message.wParam == usize::from(VK_RETURN) {
        let action = if focus == panel.control(CLOSE) {
            CLOSE
        } else {
            COPY
        };
        command(panel, action);
        return true;
    }
    // 保留只读编辑框的选区复制，有按钮焦点时快捷键复制完整 GUID。
    if message.wParam == usize::from(b'C')
        && focus != panel.control(GUID)
        && unsafe { GetKeyState(i32::from(VK_CONTROL)) } < 0
    {
        command(panel, COPY);
        return true;
    }
    false
}
