//! 已确认视觉层到共享产品宿主的薄桥；不在窗口层实现连接、授权或凭据处理。
use super::*;
use crate::product::{
    NativeAppearance, NativeLanguage, ProductAction, ProductHost, ProductPhase, ProductSnapshot,
};
use std::collections::VecDeque;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const LOG_UNITS: usize = 262_144;
const PAGE_SAMPLE_LIMIT: usize = 64;
const PAGE_BUDGET_US: u128 = 100_000;

// 各标志记录相互独立的界面事实，不构成互斥状态。
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Business {
    // 唯一业务宿主；配置、TLS 和权限都由它处理。
    host: ProductHost,
    legal_notices: Option<&'static str>,
    demo: bool,
    // 复用已有工作线程，UI 不直接打开系统剪贴板。
    clipboard: super::clipboard::Clipboard,
    started: Instant,
    last_poll: Instant,
    ticks: u64,
    max_tick_ms: u128,
    tick_host_max_ms: u128,
    tick_refresh_max_ms: u128,
    tick_prompt_max_ms: u128,
    page_max_ms: u128,
    page_prepare_max_ms: u128,
    page_fonts_max_ms: u128,
    page_controls_max_ms: u128,
    page_by_kind: BTreeMap<&'static str, u128>,
    // 同帧保留各阶段与原生操作，避免把不同帧的最大值误当成同次卡顿。
    page_max_us: u128,
    page_initial_max_us: u128,
    page_reuse_max_us: u128,
    page_frame_count: u64,
    page_slowest_frame: Option<serde_json::Value>,
    page_frame_samples: VecDeque<serde_json::Value>,
    setup_shown: bool,
    shown_tls: Option<String>,
    // 仅保存确认框用途，不把默认授权与本次会话授权混为一谈。
    pending_default: bool,
    // 原生控件重建保留尚未保存的 Relay 输入。
    draft_relay: Option<String>,
    copy_error: bool,
    copied_code: Option<String>,
    pub(super) close_text_matches: bool,
    close_text_checked: bool,
    closed: bool,
}

impl Business {
    pub(super) fn new(options: &crate::Options) -> Self {
        let mut host = ProductHost::new(options);
        host.set_elevated(unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() } != 0);
        let now = Instant::now();
        Self {
            host,
            legal_notices: options.legal_notices,
            demo: options.product_demo,
            clipboard: super::clipboard::Clipboard::default(),
            started: now,
            last_poll: now.checked_sub(POLL_INTERVAL).unwrap_or(now),
            ticks: 0,
            max_tick_ms: 0,
            tick_host_max_ms: 0,
            tick_refresh_max_ms: 0,
            tick_prompt_max_ms: 0,
            page_max_ms: 0,
            page_prepare_max_ms: 0,
            page_fonts_max_ms: 0,
            page_controls_max_ms: 0,
            page_by_kind: BTreeMap::new(),
            page_max_us: 0,
            page_initial_max_us: 0,
            page_reuse_max_us: 0,
            page_frame_count: 0,
            page_slowest_frame: None,
            page_frame_samples: VecDeque::new(),
            setup_shown: false,
            shown_tls: None,
            pending_default: false,
            draft_relay: None,
            copy_error: false,
            copied_code: None,
            close_text_matches: false,
            close_text_checked: false,
            closed: false,
        }
    }
}

fn appearance(value: NativeAppearance) -> Theme {
    match value {
        NativeAppearance::System => Theme::System,
        NativeAppearance::Light => Theme::Light,
        NativeAppearance::Dark => Theme::Dark,
    }
}

fn assign_snapshot(state: &mut State, snapshot: ProductSnapshot) {
    state.english = snapshot.language == NativeLanguage::English;
    state.theme = appearance(snapshot.appearance);
    state.connected = snapshot.controllers > 0 || snapshot.phase == ProductPhase::Controlled;
    state.configured = !snapshot.setup_needed;
    state.relay.clone_from(&snapshot.relay);
    state.default_full = snapshot.default_full;
    state.product = Some(snapshot);
}

pub(super) fn initial_state(business: &Business) -> State {
    let mut state = State {
        legal_notices: business.legal_notices,
        observer_demo: super::observer_data::ObserverData::live(),
        relay: String::new(),
        ..State::default()
    };
    assign_snapshot(&mut state, business.host.snapshot());
    state
}

// 重建前只保存编辑草稿；首次创建或失效控件不改动已保存的草稿。
pub(super) fn preserve_setup_input(panel: &Panel) {
    if panel.kind != Kind::Setup {
        return;
    }
    let input = panel.control(502);
    let text = if input.is_null() || unsafe { IsWindow(input) == 0 } {
        None
    } else {
        Some(value(input))
    };
    // GetWindowText 可能同步进入原生消息处理，必须先取文本再借用可变状态。
    let app = panel.app();
    let mut business = app.business.borrow_mut();
    let mut state = app.state.borrow_mut();
    remember_setup_input(business.as_mut(), &mut state.relay, text);
}

fn remember_setup_input(business: Option<&mut Business>, relay: &mut String, text: Option<String>) {
    let Some(text) = text else {
        return;
    };
    if let Some(business) = business {
        business.draft_relay = Some(text);
    } else {
        *relay = text;
    }
}

fn has_business(app: &App) -> bool {
    app.business.borrow().is_some()
}

fn visible_panel(app: &App, kind: Kind) -> Option<*const Panel> {
    app.panels().into_iter().find(|ptr| unsafe {
        (**ptr).kind == kind
            && IsWindow((**ptr).hwnd.get()) != 0
            && IsWindowVisible((**ptr).hwnd.get()) != 0
    })
}

fn has_modal(app: &App) -> bool {
    app.panels().into_iter().any(|ptr| unsafe {
        !matches!((*ptr).kind, Kind::Main | Kind::Observer)
            && IsWindow((*ptr).hwnd.get()) != 0
            && IsWindowVisible((*ptr).hwnd.get()) != 0
    })
}

fn update(panel: &Panel, id: i32, text: &str) {
    let hwnd = panel.control(id);
    if !hwnd.is_null() && value(hwnd) != text {
        panel.text(id, text);
    }
}

fn update_readonly(panel: &Panel, id: i32, text: &str) {
    let hwnd = panel.control(id);
    if hwnd.is_null() || value(hwnd) == text {
        return;
    }
    let (mut start, mut end) = (0u32, 0u32);
    unsafe {
        SendMessageW(
            hwnd,
            EM_GETSEL,
            &raw mut start as usize,
            &raw mut end as isize,
        );
    }
    // 用户复制或阅读已选文字时不替换正文，下一次空闲刷新会追赶。
    if start == end {
        panel.text(id, text);
    }
}

fn refresh_values(app: &App) {
    for ptr in app.panels() {
        let panel = unsafe { &*ptr };
        if unsafe { IsWindow(panel.hwnd.get()) == 0 || IsWindowVisible(panel.hwnd.get()) == 0 } {
            continue;
        }
        let ids: &[i32] = match panel.kind {
            Kind::Main if app.state.borrow().logs => &[312],
            Kind::Main => &[300, 303, 304, 305, 306, 312],
            Kind::Details => &[402, 404, 406, 408, 410],
            Kind::Permissions => &[421, 426],
            Kind::Grant => &[441],
            Kind::Exit => &[461],
            Kind::Setup => &[503, 504],
            Kind::Tls => &[526],
            Kind::About => &[542, 543],
            Kind::Observer | Kind::Licenses => &[],
        };
        for id in ids {
            if *id == 305
                && app.state.borrow().product.as_ref().is_some_and(|snapshot| {
                    matches!(
                        snapshot.phase,
                        ProductPhase::Connecting | ProductPhase::Reconnecting
                    )
                })
            {
                continue;
            }
            update(panel, *id, &text_for(panel, *id, ""));
        }
        if panel.kind == Kind::Main && app.state.borrow().logs {
            update_readonly(panel, 311, &text_for(panel, 311, ""));
        }
        if panel.kind == Kind::Tls {
            // 指纹变化必须立即替换旧选区，不能让用户对着旧证书确认新目标。
            update(panel, 524, &text_for(panel, 524, ""));
        }
        decorate(panel);
    }
}

fn snapshot_after_action(app: &App) {
    let snapshot = app
        .business
        .borrow()
        .as_ref()
        .map(|business| business.host.snapshot());
    if let Some(snapshot) = snapshot {
        if snapshot.tls.is_none()
            && let Some(business) = app.business.borrow_mut().as_mut()
        {
            business.shown_tls = None;
        }
        let changed = {
            let mut state = app.state.borrow_mut();
            let active_changed = state
                .observer_demo
                .ingest(Vec::new(), &snapshot.active_operations);
            let dropped_changed = state
                .observer_demo
                .set_dropped(snapshot.dropped_observations);
            assign_snapshot(&mut state, snapshot);
            active_changed || dropped_changed
        };
        // 主动停止／重新连接可能不再有后续输出，动作快照必须立即清除旧活动亮点。
        if changed && let Some(ptr) = visible_panel(app, Kind::Observer) {
            sync_output(unsafe { &*ptr }, false);
        }
    }
}

fn perform(app: &App, action: ProductAction) -> bool {
    let result = {
        let mut slot = app.business.borrow_mut();
        let Some(business) = slot.as_mut() else {
            return false;
        };
        business.host.action(action)
    };
    let success = result.is_ok();
    app.state.borrow_mut().business_error = result.err().unwrap_or_default();
    snapshot_after_action(app);
    refresh_values(app);
    success
}

fn save_draft(app: &App) {
    let panel = visible_panel(app, Kind::Setup);
    let draft = panel.map(|ptr| value(unsafe { &*ptr }.control(502)));
    if let Some(business) = app.business.borrow_mut().as_mut() {
        business.draft_relay = draft;
    }
}

fn dismiss(panel: &Panel) {
    super::close_panel_raw(panel);
}

pub(super) fn poll(app: &App) -> bool {
    let started = Instant::now();
    let reading_logs = app.state.borrow().logs;
    let batch = {
        let mut slot = app.business.borrow_mut();
        let Some(business) = slot.as_mut() else {
            return false;
        };
        if business.closed || business.last_poll.elapsed() < POLL_INTERVAL {
            return true;
        }
        business.last_poll = Instant::now();
        business.ticks = business.ticks.saturating_add(1);
        let output = business.host.tick();
        let mut snapshot = business.host.snapshot();
        if reading_logs
            && snapshot.unread > 0
            && business.host.action(ProductAction::MarkLogsRead).is_ok()
        {
            snapshot = business.host.snapshot();
        }
        business
            .clipboard
            .cancel_unless(snapshot.code.as_deref().filter(|_| snapshot.can_copy));
        let copied = business.clipboard.poll();
        let clear_copy =
            !snapshot.can_copy || business.copied_code.as_deref() != snapshot.code.as_deref();
        if clear_copy {
            business.copied_code = None;
        }
        if copied == Some(super::clipboard::Outcome::Copied) {
            business.copied_code.clone_from(&snapshot.code);
        }
        let mut copy_message = None;
        if copied == Some(super::clipboard::Outcome::Failed) {
            business.copy_error = true;
            copy_message = Some(
                snapshot
                    .t("复制失败，请重试。", "Copy failed; please retry.")
                    .to_owned(),
            );
        } else if copied == Some(super::clipboard::Outcome::Copied) && business.copy_error {
            business.copy_error = false;
            copy_message = Some(String::new());
        }
        (output, snapshot, copied, copy_message, clear_copy)
    };
    let host_elapsed = started.elapsed();
    let refresh_started = Instant::now();
    let (output, snapshot, copied, copy_message, clear_copy) = batch;
    let (appearance_changed, observation_changed, connection_changed) = {
        let mut state = app.state.borrow_mut();
        let connection_changed = state.connected
            != (snapshot.controllers > 0 || snapshot.phase == ProductPhase::Controlled);
        let appearance_changed = state.english != (snapshot.language == NativeLanguage::English)
            || state.theme != appearance(snapshot.appearance);
        let changed_output = state
            .observer_demo
            .ingest(output, &snapshot.active_operations);
        let changed_dropped = state
            .observer_demo
            .set_dropped(snapshot.dropped_observations);
        let changed = changed_output || changed_dropped;
        if clear_copy {
            state.copied = None;
        }
        if copied == Some(super::clipboard::Outcome::Copied) {
            state.copied = Some(Instant::now());
        }
        if state
            .copied
            .is_some_and(|when| when.elapsed() >= Duration::from_secs(3))
        {
            state.copied = None;
        }
        if let Some(error) = copy_message {
            state.business_error = error;
        }
        assign_snapshot(&mut state, snapshot);
        (appearance_changed, changed, connection_changed)
    };
    if appearance_changed {
        save_draft(app);
        app.refresh();
    } else {
        refresh_values(app);
    }
    if observation_changed && let Some(ptr) = visible_panel(app, Kind::Observer) {
        sync_output(unsafe { &*ptr }, false);
    }

    if connection_changed {
        unsafe {
            InvalidateRect(app.main.get(), null(), 0);
        }
    }
    let refresh_elapsed = refresh_started.elapsed();
    let prompt_started = Instant::now();
    let (setup, tls_key) = {
        let state = app.state.borrow();
        let snapshot = state.product.as_ref().expect("业务快照已初始化");
        (
            snapshot.setup_needed,
            snapshot
                .tls
                .as_ref()
                .map(|tls| format!("{}\n{}", tls.relay, tls.fingerprint)),
        )
    };
    let no_modal = !has_modal(app);
    let (show_setup, show_tls) = {
        let mut slot = app.business.borrow_mut();
        let business = slot.as_mut().expect("业务宿主仍在");
        if tls_key.is_none() {
            business.shown_tls = None;
        }
        let show_tls = no_modal && tls_key.is_some() && business.shown_tls != tls_key;
        if show_tls {
            business.shown_tls = tls_key;
        }
        let show_setup = no_modal && !show_tls && setup && !business.setup_shown;
        if show_setup {
            business.setup_shown = true;
        }
        (show_setup, show_tls)
    };
    if show_tls {
        app.open(Kind::Tls, app.main.get());
    } else if show_setup {
        app.open(Kind::Setup, app.main.get());
    }
    if let Some(business) = app.business.borrow_mut().as_mut() {
        business.max_tick_ms = business.max_tick_ms.max(started.elapsed().as_millis());
        business.tick_host_max_ms = business.tick_host_max_ms.max(host_elapsed.as_millis());
        business.tick_refresh_max_ms = business
            .tick_refresh_max_ms
            .max(refresh_elapsed.as_millis());
        business.tick_prompt_max_ms = business
            .tick_prompt_max_ms
            .max(prompt_started.elapsed().as_millis());
    }
    true
}

pub(super) fn command(panel: &Panel, id: i32) -> bool {
    let app = panel.app();
    if !has_business(app) {
        return false;
    }
    match id {
        304 => {
            app.state.borrow_mut().copied = None;
            let snapshot = app.state.borrow().product.clone().expect("业务快照");
            if snapshot.can_copy
                && let Some(code) = snapshot.code.as_deref()
            {
                let result = app
                    .business
                    .borrow_mut()
                    .as_mut()
                    .expect("业务宿主")
                    .clipboard
                    .request(code);
                if result.is_err() {
                    app.business
                        .borrow_mut()
                        .as_mut()
                        .expect("业务宿主")
                        .copy_error = true;
                    app.state.borrow_mut().business_error = snapshot
                        .t("复制失败，请重试。", "Copy failed; please retry.")
                        .into();
                }
                refresh_values(app);
            }
        }
        323 => {
            let (directory, error) = {
                let state = app.state.borrow();
                let snapshot = state.product.as_ref().expect("业务快照");
                (
                    snapshot.transfer_root.clone(),
                    snapshot
                        .t(
                            "无法打开文件接收目录",
                            "Unable to open received files folder",
                        )
                        .to_owned(),
                )
            };
            if !directory.trim().is_empty() {
                // 目录仅由既有 Agent 核心维护；这里按用户动作打开，不创建目录或复制路径。
                let result = unsafe {
                    windows_sys::Win32::UI::Shell::ShellExecuteW(
                        app.main.get(),
                        wide("open").as_ptr(),
                        wide(&directory).as_ptr(),
                        null(),
                        null(),
                        SW_SHOWNORMAL,
                    )
                };
                app.state.borrow_mut().business_error = if result as usize <= 32 {
                    error
                } else {
                    String::new()
                };
                refresh_values(app);
            }
        }
        308 => {
            perform(app, ProductAction::MarkLogsRead);
            return false; // 继续由现有视觉层切到日志页。
        }
        329 => {
            if perform(app, ProductAction::ClearLogs) {
                update(panel, 311, "");
            }
        }
        422 => {
            perform(app, ProductAction::RequestControl { full: false });
        }
        423 => {
            let snapshot = app.state.borrow().product.clone().expect("业务快照");
            if snapshot.control_enabled && !snapshot.control_pending {
                app.business
                    .borrow_mut()
                    .as_mut()
                    .expect("业务宿主")
                    .pending_default = false;
                app.open(Kind::Grant, panel.hwnd.get());
            }
        }
        424 => {
            let checked = unsafe { SendMessageW(panel.control(424), BM_GETCHECK, 0, 0) } != 0;
            if checked {
                app.business
                    .borrow_mut()
                    .as_mut()
                    .expect("业务宿主")
                    .pending_default = true;
                decorate(panel);
                app.open(Kind::Grant, panel.hwnd.get());
            } else {
                perform(app, ProductAction::SaveDefaultControl { full: false });
            }
        }
        443 => {
            let default = app
                .business
                .borrow()
                .as_ref()
                .expect("业务宿主")
                .pending_default;
            let action = if default {
                ProductAction::SaveDefaultControl { full: true }
            } else {
                ProductAction::RequestControl { full: true }
            };
            if perform(app, action) {
                app.business
                    .borrow_mut()
                    .as_mut()
                    .expect("业务宿主")
                    .pending_default = false;
                dismiss(panel);
            }
        }
        506 => {
            let relay = value(panel.control(502));
            if perform(app, ProductAction::SaveSetup { relay }) {
                app.business
                    .borrow_mut()
                    .as_mut()
                    .expect("业务宿主")
                    .draft_relay = None;
                dismiss(panel);
            }
        }
        521..=523 => {
            let action = match id {
                521 => ProductAction::TrustCancel,
                522 => ProductAction::TrustOnce,
                _ => ProductAction::TrustSave,
            };
            if perform(app, action) {
                dismiss(panel);
            }
        }
        507 | 508 => {
            save_draft(app);
            perform(
                app,
                ProductAction::SetLanguage(if id == 508 {
                    NativeLanguage::English
                } else {
                    NativeLanguage::Chinese
                }),
            );
            app.refresh();
        }
        463 => {
            if prepare_shutdown(app) {
                app.shutdown();
            }
        }
        220 | 221 => {} // 合成演示控制不进入业务。
        _ => return false,
    }
    true
}

pub(super) fn close(panel: &Panel) -> bool {
    let app = panel.app();
    if !has_business(app) {
        return false;
    }
    match panel.kind {
        Kind::Tls => {
            if perform(app, ProductAction::TrustCancel) {
                dismiss(panel);
            }
            true
        }
        Kind::Grant => {
            app.business
                .borrow_mut()
                .as_mut()
                .expect("业务宿主")
                .pending_default = false;
            dismiss(panel);
            refresh_values(app);
            true
        }
        Kind::Setup => {
            app.business
                .borrow_mut()
                .as_mut()
                .expect("业务宿主")
                .draft_relay = None;
            dismiss(panel);
            true
        }
        _ => false,
    }
}

pub(super) fn menu(panel: &Panel, id: i32) -> bool {
    let app = panel.app();
    if !has_business(app) {
        return false;
    }
    match id {
        5001 | 5002 => {
            save_draft(app);
            perform(
                app,
                ProductAction::SetLanguage(if id == 5002 {
                    NativeLanguage::English
                } else {
                    NativeLanguage::Chinese
                }),
            );
            app.refresh();
        }
        5010..=5012 => {
            save_draft(app);
            perform(
                app,
                ProductAction::SetAppearance(match id {
                    5010 => NativeAppearance::System,
                    5011 => NativeAppearance::Light,
                    _ => NativeAppearance::Dark,
                }),
            );
            app.refresh();
        }
        5020 => {
            app.open(Kind::Permissions, panel.hwnd.get());
        }
        5021 => {
            app.open(Kind::About, panel.hwnd.get());
        }
        5022 => {
            app.open(Kind::Setup, panel.hwnd.get());
        }
        5043 => app.observer(),
        5030..=5046 => {}
        _ => return false,
    }
    true
}

fn logs(snapshot: &ProductSnapshot, filter: usize) -> String {
    let mut rows = Vec::new();
    let mut units = 0;
    let mut trimmed = false;
    for log in snapshot.logs.iter().rev().filter(|log| match filter {
        1 => log.level == crate::product::LogLevel::Running,
        2 => log.level == crate::product::LogLevel::Success,
        3 => log.level == crate::product::LogLevel::Error,
        4 => log.level == crate::product::LogLevel::Info,
        _ => true,
    }) {
        let row = format!(
            "{} [{}] {} {}",
            log.occurred, log.request, log.source, log.message
        )
        .replace('\0', "\u{2400}");
        let size = row.encode_utf16().count() + 2;
        if units + size > LOG_UNITS {
            trimmed = true;
            break;
        }
        units += size;
        rows.push(row);
    }
    rows.reverse();
    if trimmed {
        rows.insert(
            0,
            snapshot
                .t(
                    "[仅显示最近日志；较早记录已省略]",
                    "[Recent logs only; older records omitted]",
                )
                .into(),
        );
    }
    presentation_lines(&rows)
}

pub(super) fn text_for(panel: &Panel, id: i32, original: &str) -> String {
    let app = panel.app();
    let state = app.state.borrow();
    let Some(snapshot) = state.product.as_ref() else {
        return original.into();
    };
    let t = |zh, en| snapshot.t(zh, en);
    match id {
        460 => if state.english { format!("Exit {}?", snapshot.product_name) } else { format!("确认退出 {}？", snapshot.product_name) },
        500 => if state.english { format!("Welcome to {}", snapshot.product_name) } else { format!("欢迎使用 {}", snapshot.product_name) },
        540 => snapshot.product_name.clone(),
        300 => snapshot.status.clone(),
        303 => snapshot.code.clone().unwrap_or_else(|| "--- --- ---".into()),
        304 => if state.copied.is_some() { "✓".into() } else { t("复制控制码", "Copy code").into() },
        305 => snapshot.countdown.clone(),
        306 => state.activity_view.text(snapshot.language),
        312 | 504 => if state.business_error.is_empty() { snapshot.detail.clone() } else { state.business_error.clone() },
        311 if panel.kind == Kind::Main => logs(snapshot, state.filter),
        410 => state.business_error.clone(),
        402 => snapshot.relay.clone(),
        404 => t(if snapshot.elevated { "管理员" } else { "普通用户" }, if snapshot.elevated { "Administrator" } else { "Standard user" }).into(),
        406 => snapshot.transfer_root.clone(),
        408 => snapshot.capabilities.iter().filter(|(_, enabled)| *enabled).map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(" · "),
        421 => if snapshot.control_source.is_empty() { t("由本机用户决定工程师可以执行的操作。", "The local user controls permitted operations.").into() } else { format!("{}：{}", t("授权来源", "Permission source"), snapshot.control_source) },
        424 => t("以后默认允许完全控制", "Allow full control by default").into(),
        426 => {
            let mut text = snapshot.control_summary.clone();
            if snapshot.control_pending { text.push_str(t("\r\n正在等待权限确认", "\r\nWaiting for permission confirmation")); }
            if let Some(error) = &snapshot.control_error { text.push_str("\r\n"); text.push_str(error); }
            if !state.business_error.is_empty() { text.push_str("\r\n"); text.push_str(&state.business_error); }
            text
        }
        441 => {
            let default = app.business.borrow().as_ref().is_some_and(|business| business.pending_default);
            let text = if default { t("以后新连接将默认允许完全控制。\r\n工程师可以执行命令、SSH、串口和文件操作。\r\n仅在可信维护环境中允许。", "Future connections will allow full control by default.\r\nCommands, SSH, serial and file operations will be permitted.\r\nAllow only in a trusted maintenance environment.") }
                else { t("本次会话将申请完全控制权限。\r\n工程师可执行命令、SSH、串口和文件操作。\r\n权限以服务端确认结果为准。", "Full control will be requested for this session.\r\nCommands, SSH, serial and file operations may be performed.\r\nPermission depends on the confirmed server result.") };
            if state.business_error.is_empty() { text.into() } else { format!("{text}\r\n{}", state.business_error) }
        }
        461 => {
            let text = if state.english { format!("Exiting stops assistance and disconnects this computer.\r\n\r\nConnected controllers: {}\r\nRunning operations: {}", snapshot.controllers, snapshot.running_count) }
                else { format!("退出后，工程师将无法继续连接此电脑。\r\n\r\n已连接控制端：{}\r\n正在执行操作：{}", snapshot.controllers, snapshot.running_count) };
            if state.business_error.is_empty() { text } else { format!("{text}\r\n{}", state.business_error) }
        }
        502 => app.business.borrow().as_ref().and_then(|business| business.draft_relay.clone()).unwrap_or_else(|| snapshot.relay.clone()),
        503 => format!("{}\r\n{}：{}", t("保存后将建立真实连接并获取控制码。", "Saving establishes a real connection and obtains a pairing code."), t("配置文件", "Configuration"), snapshot.config_path),
        506 => t("保存并连接", "Save and connect").into(),
        523 => t("信任并保存", "Trust and save").into(),
        524 => snapshot.tls.as_ref().map_or_else(String::new, |tls| format!("Relay: {}\r\nSHA256: {}\r\n{}", tls.relay, tls.fingerprint, tls.reason)),
        526 => if state.business_error.is_empty() { snapshot.tls.as_ref().and_then(|tls| tls.error.clone()).unwrap_or_else(|| t("请通过可信渠道核对指纹后再继续。", "Verify the fingerprint using a trusted channel.").into()) } else { state.business_error.clone() },
        542 => t("Windows 原生界面；复用现有 RemoteOps 业务核心。\r\n实际连接、权限和操作结果由共享宿主提供。", "Windows native interface using the existing RemoteOps core.\r\nThe shared host provides actual connection, permission and operation results.").into(),
        543 => format!("{} v{}", snapshot.product_name, snapshot.version),
        _ => original.into(),
    }
}

pub(super) fn decorate(panel: &Panel) {
    let app = panel.app();
    let data = {
        let state = app.state.borrow();
        state.product.as_ref().map(|snapshot| {
            (
                snapshot.can_copy,
                snapshot.control_enabled && !snapshot.control_pending,
                snapshot.default_full,
                snapshot.endpoint_editable,
                snapshot.tls.is_some(),
                !snapshot.transfer_root.trim().is_empty(),
            )
        })
    };
    let Some((can_copy, can_control, default_full, endpoint_editable, has_tls, has_transfer_root)) =
        data
    else {
        return;
    };
    unsafe {
        for (id, enabled) in [
            (304, can_copy),
            (323, has_transfer_root),
            (422, can_control),
            (423, can_control),
            (506, !has_tls),
            (522, has_tls),
            (523, has_tls),
        ] {
            let hwnd = panel.control(id);
            if !hwnd.is_null() && (IsWindowEnabled(hwnd) != 0) != enabled {
                EnableWindow(hwnd, i32::from(enabled));
            }
        }
        let checkbox = panel.control(424);
        if !checkbox.is_null() && (SendMessageW(checkbox, BM_GETCHECK, 0, 0) != 0) != default_full {
            SendMessageW(checkbox, BM_SETCHECK, usize::from(default_full), 0);
        }
        let input = panel.control(502);
        if !input.is_null() {
            if (IsWindowEnabled(input) != 0) != endpoint_editable {
                EnableWindow(input, i32::from(endpoint_editable));
            }
            if (GetWindowLongPtrW(input, GWL_STYLE) as u32 & ES_READONLY as u32 != 0)
                == endpoint_editable
            {
                SendMessageW(input, EM_SETREADONLY, usize::from(!endpoint_editable), 0);
            }
        }
    }
}

fn page_kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Main => "main",
        Kind::Details => "details",
        Kind::Permissions => "permissions",
        Kind::Grant => "grant",
        Kind::Exit => "exit",
        Kind::Setup => "setup",
        Kind::Tls => "tls",
        Kind::Observer => "observer",
        Kind::About => "about",
        Kind::Licenses => "licenses",
    }
}

fn record_page_frame(business: &mut Business, kind: Kind, frame: super::render_plan::PageFrame) {
    let name = page_kind_name(kind);
    // 保留原先整桶口径；各阶段来自本帧，先相加再换算毫秒。
    let controls_us = frame
        .declare_us
        .saturating_add(frame.decorate_us)
        .saturating_add(frame.restore_us)
        .saturating_add(frame.prune_us);
    business.page_max_ms = business.page_max_ms.max(frame.total_us / 1_000);
    business.page_prepare_max_ms = business.page_prepare_max_ms.max(frame.prepare_us / 1_000);
    business.page_fonts_max_ms = business.page_fonts_max_ms.max(frame.fonts_us / 1_000);
    business.page_controls_max_ms = business.page_controls_max_ms.max(controls_us / 1_000);
    let maximum = business.page_by_kind.entry(name).or_default();
    *maximum = (*maximum).max(frame.total_us / 1_000);
    let phase_maximum = if frame.initial {
        &mut business.page_initial_max_us
    } else {
        &mut business.page_reuse_max_us
    };
    *phase_maximum = (*phase_maximum).max(frame.total_us);
    business.page_frame_count = business.page_frame_count.saturating_add(1);
    // 仅固定窗口类别、耗时与计数进入报告，不采集正文、路径或控件标识。
    let sample = serde_json::json!({
        "kind": name,
        "initial": frame.initial,
        "total_us": frame.total_us,
        "prepare_us": frame.prepare_us,
        "fonts_us": frame.fonts_us,
        "declare_us": frame.declare_us,
        "decorate_us": frame.decorate_us,
        "restore_us": frame.restore_us,
        "prune_us": frame.prune_us,
        "work": frame.work,
    });
    if business.page_slowest_frame.is_none() || frame.total_us > business.page_max_us {
        business.page_slowest_frame = Some(sample.clone());
    }
    business.page_max_us = business.page_max_us.max(frame.total_us);
    if business.page_frame_samples.len() == PAGE_SAMPLE_LIMIT {
        business.page_frame_samples.pop_front();
    }
    business.page_frame_samples.push_back(sample);
}

pub(super) fn page_rendered(app: &App, kind: Kind, frame: super::render_plan::PageFrame) {
    if let Some(business) = app.business.borrow_mut().as_mut() {
        record_page_frame(business, kind, frame);
    }
}

fn page_within_budget(business: &Business) -> bool {
    business.page_max_us <= PAGE_BUDGET_US
}

fn native_paint_check(max_us: u128, count: u64) -> Option<crate::Check> {
    (count > 0).then(|| crate::Check {
        name: "native_paint_responsive".into(),
        passed: max_us <= 100_000,
        detail: "实际原生绘制不超过100毫秒；独立验收，不替代页面构建及布局计时".into(),
    })
}

pub(super) fn prepare_shutdown(app: &App) -> bool {
    if !has_business(app) {
        return true;
    }
    if app
        .business
        .borrow()
        .as_ref()
        .is_some_and(|business| business.closed)
    {
        return true;
    }
    let observer = app
        .panels()
        .into_iter()
        .find(|ptr| unsafe { (**ptr).kind == Kind::Observer && IsWindow((**ptr).hwnd.get()) != 0 });
    let matches = observer.map(|ptr| {
        let panel = unsafe { &*ptr };
        sync_output(panel, true);
        value(panel.control(202)) == super::observer_text(&app.state.borrow())
    });
    {
        let mut slot = app.business.borrow_mut();
        let business = slot.as_mut().expect("业务宿主");
        business.close_text_checked = matches.is_some();
        business.close_text_matches = matches.unwrap_or(false);
    }
    if !perform(app, ProductAction::Stop) {
        return false;
    }
    let clean = {
        let mut slot = app.business.borrow_mut();
        let business = slot.as_mut().expect("业务宿主");
        business.clipboard.shutdown();
        let clean = business.host.shutdown();
        business.closed = clean;
        clean
    };
    if !clean {
        app.state.borrow_mut().business_error = "后台尚未结束，请稍后重试。".into();
        refresh_values(app);
    }
    clean
}

pub(super) fn report(app: &App) -> crate::Report {
    let slot = app.business.borrow();
    let business = slot.as_ref().expect("业务报告只用于业务入口");
    let state = app.state.borrow();
    let snapshot = state.product.as_ref().expect("业务快照");
    let mut checks = vec![crate::Check {
        name: "background_stopped".into(),
        passed: business.closed,
        detail: "共享宿主确认后台已停止".into(),
    }];
    if business.close_text_checked {
        checks.push(crate::Check {
            name: "observer_text_matches_model_on_close".into(),
            passed: business.close_text_matches,
            detail: "销毁前实际读取原生 EDIT 正文与当前筛选模型比较".into(),
        });
    }
    if business.page_frame_count > 0 {
        checks.push(crate::Check {
            name: "product_page_responsive".into(),
            passed: page_within_budget(business),
            detail: "实际窗口构建及布局不超过100毫秒".into(),
        });
    }
    let (native_paint_max_us, native_paint_count) = super::render_plan::paint_metrics();
    if let Some(check) = native_paint_check(native_paint_max_us, native_paint_count) {
        checks.push(check);
    }
    if business.ticks > 0 {
        checks.push(crate::Check {
            name: "ui_responsive".into(),
            passed: business.max_tick_ms <= 100,
            detail: "实际业务轮询及窗口刷新不超过100毫秒".into(),
        });
    }
    let mut metrics = serde_json::json!({"max_tick_ms":business.max_tick_ms,"product_page_max_ms":business.page_max_ms,"ui_ticks":business.ticks,"elapsed_ms":business.started.elapsed().as_millis(),"observer_text_matches_model_on_close":business.close_text_checked.then_some(business.close_text_matches),"rendered_text_matches_model_on_close":business.close_text_checked.then_some(business.close_text_matches),"retained_records":state.observer_demo.total_count(),"received_lines":state.observer_demo.total_received(),"trimmed_lines":state.observer_demo.trimmed_count(),"dropped_observations":snapshot.dropped_observations,"running_count":snapshot.running_count,"agent_core_included":cfg!(feature="agent-runtime")});
    metrics["tick_host_max_ms"] = serde_json::json!(business.tick_host_max_ms);
    metrics["tick_refresh_max_ms"] = serde_json::json!(business.tick_refresh_max_ms);
    metrics["tick_prompt_max_ms"] = serde_json::json!(business.tick_prompt_max_ms);
    metrics["page_prepare_max_ms"] = serde_json::json!(business.page_prepare_max_ms);
    metrics["page_fonts_max_ms"] = serde_json::json!(business.page_fonts_max_ms);
    metrics["page_controls_max_ms"] = serde_json::json!(business.page_controls_max_ms);
    metrics["product_page_max_us"] = serde_json::json!(business.page_max_us);
    metrics["page_initial_max_us"] = serde_json::json!(business.page_initial_max_us);
    metrics["page_reuse_max_us"] = serde_json::json!(business.page_reuse_max_us);
    metrics["page_frame_count"] = serde_json::json!(business.page_frame_count);
    metrics["page_slowest_frame"] = serde_json::json!(business.page_slowest_frame);
    metrics["page_frame_samples"] = serde_json::json!(business.page_frame_samples);
    metrics["native_paint_max_us"] = serde_json::json!(native_paint_max_us);
    metrics["native_paint_count"] = serde_json::json!(native_paint_count);
    for (kind, maximum) in &business.page_by_kind {
        metrics[format!("page_{kind}_max_ms")] = serde_json::json!(maximum);
    }
    crate::Report {
        schema_version: 1,
        passed: checks.iter().all(|check| check.passed),
        checks,
        metrics,
        environment: serde_json::json!({"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"real_business_ui":!business.demo,"product_demo":business.demo,"rdp_dpi_verified":false}),
        limitations: vec!["真实交换机、物理串口、RDP和显示缩放仍待现场验收".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::super::render_plan::{ControlWork, PageFrame};
    use super::*;
    use remoteops_agent_desktop_host::{LogLevel, ProductLog};

    fn page_business() -> Business {
        Business::new(&crate::Options {
            product_demo: true,
            ..crate::Options::default()
        })
    }

    #[test]
    fn native_paint_budget_requires_samples_and_uses_microseconds() {
        assert!(native_paint_check(0, 0).is_none());
        assert!(native_paint_check(100_000, 1).unwrap().passed);
        assert!(!native_paint_check(100_001, 1).unwrap().passed);
    }

    #[test]
    fn slowest_page_keeps_one_frame_instead_of_mixing_stage_maxima() {
        let mut business = page_business();
        let slow = PageFrame {
            initial: true,
            total_us: 120_000,
            prepare_us: 1_000,
            declare_us: 110_000,
            work: ControlWork {
                created: 5,
                create_us: 109_000,
                ..ControlWork::default()
            },
            ..PageFrame::default()
        };
        record_page_frame(&mut business, Kind::Observer, slow);
        record_page_frame(
            &mut business,
            Kind::Main,
            PageFrame {
                total_us: 90_000,
                prepare_us: 80_000,
                declare_us: 2_000,
                work: ControlWork {
                    reused: 8,
                    ..ControlWork::default()
                },
                ..PageFrame::default()
            },
        );
        let retained = business.page_slowest_frame.as_ref().unwrap();
        assert_eq!(retained["kind"], "observer");
        assert_eq!(retained["total_us"], 120_000);
        assert_eq!(retained["prepare_us"], 1_000);
        assert_eq!(retained["work"]["created"], 5);
        assert_eq!(retained["work"]["reused"], 0);
        assert_eq!(business.page_prepare_max_ms, 80);
        assert_eq!(business.page_controls_max_ms, 110);
        assert_eq!(business.page_initial_max_us, 120_000);
        assert_eq!(business.page_reuse_max_us, 90_000);
        assert_eq!(business.page_by_kind["main"], 90);
        assert!(!page_within_budget(&business));
    }

    #[test]
    fn page_budget_uses_microseconds_and_keeps_initial_frames() {
        let mut business = page_business();
        for (total_us, expected) in [(999, true), (100_000, true), (100_001, false)] {
            record_page_frame(
                &mut business,
                Kind::Main,
                PageFrame {
                    initial: true,
                    total_us,
                    ..PageFrame::default()
                },
            );
            assert_eq!(page_within_budget(&business), expected);
        }
        record_page_frame(&mut business, Kind::Main, PageFrame::default());
        // 旧毫秒口径仍为 100，但严格门槛必须保留最初超出的 1 微秒。
        assert_eq!(business.page_max_ms, 100);
        assert_eq!(business.page_max_us, 100_001);
        assert_eq!(business.page_initial_max_us, 100_001);
        assert_eq!(business.page_reuse_max_us, 0);
        assert_eq!(business.page_frame_count, 4);
        assert!(!page_within_budget(&business));
    }

    #[test]
    fn page_control_bucket_sums_same_frame_before_millisecond_conversion() {
        let mut business = page_business();
        record_page_frame(
            &mut business,
            Kind::Observer,
            PageFrame {
                total_us: 1_000,
                declare_us: 250,
                decorate_us: 250,
                restore_us: 250,
                prune_us: 250,
                ..PageFrame::default()
            },
        );
        assert_eq!(business.page_controls_max_ms, 1);
        assert_eq!(business.page_frame_count, 1);
    }

    #[test]
    fn page_samples_evict_old_frames_but_keep_the_global_slowest() {
        let mut business = page_business();
        record_page_frame(
            &mut business,
            Kind::Setup,
            PageFrame {
                initial: true,
                total_us: 200_000,
                ..PageFrame::default()
            },
        );
        for total_us in 1..=65 {
            record_page_frame(
                &mut business,
                Kind::Observer,
                PageFrame {
                    total_us,
                    ..PageFrame::default()
                },
            );
        }
        assert_eq!(business.page_frame_samples.len(), PAGE_SAMPLE_LIMIT);
        assert_eq!(business.page_frame_samples.front().unwrap()["total_us"], 2);
        assert_eq!(business.page_frame_samples.back().unwrap()["total_us"], 65);
        assert_eq!(business.page_frame_count, 66);
        assert_eq!(
            business.page_slowest_frame.as_ref().unwrap()["kind"],
            "setup"
        );
        assert!(!page_within_budget(&business));
    }

    #[test]
    fn real_initial_state_never_contains_synthetic_observer_output() {
        let business = Business::new(&crate::Options {
            product_demo: true,
            ..crate::Options::default()
        });
        let state = initial_state(&business);
        assert!(state.product.is_some());
        assert_eq!(state.observer_demo.rows(), Vec::<String>::new());
        assert_eq!(state.observer_demo.total_running(), 0);
        assert!(!state.relay.contains("example.test"));
    }
    #[test]
    fn setup_rebuild_preserves_empty_and_unicode_drafts_without_saving_host_config() {
        let mut business = Business::new(&crate::Options {
            product_demo: true,
            ..crate::Options::default()
        });
        let original = business.host.snapshot();
        let mut relay = original.relay.clone();
        for input in ["", "网关.example:443/中文 空格"] {
            remember_setup_input(Some(&mut business), &mut relay, Some(input.into()));
            assert_eq!(business.draft_relay.as_deref(), Some(input));
            let after = business.host.snapshot();
            assert_eq!(after.relay, original.relay);
            assert_eq!(after.config_path, original.config_path);
            assert_eq!(relay, original.relay);
        }
    }

    #[test]
    fn setup_first_creation_keeps_existing_draft() {
        let mut business = Business::new(&crate::Options {
            product_demo: true,
            ..crate::Options::default()
        });
        business.draft_relay = Some("尚未保存的中文地址".into());
        let mut relay = "已配置地址".into();
        remember_setup_input(Some(&mut business), &mut relay, None);
        assert_eq!(business.draft_relay.as_deref(), Some("尚未保存的中文地址"));
        assert_eq!(relay, "已配置地址");
    }

    #[test]
    fn standalone_setup_rebuild_keeps_input_and_empty_value() {
        let mut relay = "初始地址".into();
        remember_setup_input(None, &mut relay, Some("演示 Relay 中文 空格".into()));
        assert_eq!(relay, "演示 Relay 中文 空格");
        remember_setup_input(None, &mut relay, None);
        assert_eq!(relay, "演示 Relay 中文 空格");
        remember_setup_input(None, &mut relay, Some(String::new()));
        assert_eq!(relay, "");
    }

    #[test]
    fn activity_log_filter_is_structured_and_keeps_output_literal() {
        let mut snapshot = ProductSnapshot::default();
        for (level, message) in [
            (LogLevel::Info, "Running is ordinary output"),
            (LogLevel::Running, "start"),
            (LogLevel::Success, "done"),
            (LogLevel::Error, "failed"),
        ] {
            snapshot.record(ProductLog {
                record_id: None,
                occurred: "12:00:00".into(),
                request: "task".into(),
                source: "CMD".into(),
                level,
                message: message.into(),
            });
        }
        assert!(logs(&snapshot, 1).contains("start"));
        assert!(!logs(&snapshot, 1).contains("ordinary"));
        assert!(logs(&snapshot, 4).contains("ordinary"));
        assert!(!logs(&snapshot, 4).contains("failed"));
    }
    #[test]
    fn bounded_activity_text_keeps_recent_rows_and_visible_nul() {
        let mut snapshot = ProductSnapshot::default();
        for index in 0..100 {
            snapshot.record(ProductLog {
                record_id: None,
                occurred: String::new(),
                request: String::new(),
                source: String::new(),
                level: LogLevel::Info,
                message: format!("{index:03} {}\0", "汉".repeat(8000)),
            });
        }
        let text = logs(&snapshot, 0);
        assert!(text.contains("099"));
        assert!(!text.contains("000"));
        assert!(text.contains('\u{2400}'));
        assert!(!text.contains('\0'));
        assert!(text.encode_utf16().count() < LOG_UNITS + 100);
    }
}
