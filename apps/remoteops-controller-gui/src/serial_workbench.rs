//! 独立的远程串口工作台。
#![allow(
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::too_many_lines,
    clippy::assigning_clones,
    clippy::semicolon_if_nothing_returned,
    clippy::struct_excessive_bools
)]

use std::collections::VecDeque;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use eframe::egui::{
    Align, ComboBox, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui, Vec2,
};
use egui_phosphor::regular as icons;
use remoteops_domain::{
    ApprovalId, RemoteOperation, SerialDataBits, SerialFlowControl, SerialParity, SerialSettings,
    SerialStopBits, SessionId,
};
use remoteops_i18n::Translator;

use crate::{
    backend::{
        ApprovalCompletion, BackendCommand, BackendEvent, PendingApproval, SerialPortDescriptor,
    },
    theme::Palette,
    widgets::{ButtonKind, card, icon, icon_button, styled_button},
};

const MAX_TERMINAL_LINES: usize = 2_000;
const MAX_TERMINAL_CHARS: usize = 128_000;
const MAX_AI_CONTEXT_CHARS: usize = 16_000;
const MAX_SERIAL_WRITE_BYTES: usize = 64 * 1024;
const MAX_APPROVAL_PREVIEW_BYTES: usize = 64;
const MAX_APPROVAL_PREVIEW_CHARS: usize = 240;

/// 串口数据显示模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SerialDisplayMode {
    Text,
    Hex,
}

/// 人工串口输入的换行方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding {
    None,
    Cr,
    Lf,
    CrLf,
}

impl LineEnding {
    fn label(self, translator: &Translator) -> String {
        match self {
            Self::None => translator.text("serial.line_ending.none"),
            Self::Cr => "CR".to_owned(),
            Self::Lf => "LF".to_owned(),
            Self::CrLf => "CRLF".to_owned(),
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalSource {
    System,
    Manual,
    Device,
    DeviceError,
}

impl TerminalSource {
    fn context_label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Manual => "manual",
            Self::Device => "device",
            Self::DeviceError => "device-error",
        }
    }
}

#[derive(Clone, Debug)]
struct TerminalLine {
    source: TerminalSource,
    text: String,
    hex: String,
}

/// 串口独立窗口的全部交互状态。
pub struct SerialWorkbench {
    /// 是否显示独立原生窗口。
    pub open: bool,
    session_id: Option<SessionId>,
    serial_session_id: Option<String>,
    port_name: String,
    ports: Vec<SerialPortDescriptor>,
    settings: SerialSettings,
    writable: bool,
    connected_writable: bool,
    opening: bool,
    display_mode: SerialDisplayMode,
    input_hex: bool,
    line_ending: LineEnding,
    input: String,
    terminal: VecDeque<TerminalLine>,
    terminal_chars: usize,
    follow_output: bool,
    paused: bool,
    ai_prompt: String,
    ai_answer: String,
    ai_busy: bool,
    ai_allow_readonly_queries: bool,
    pending_approvals: VecDeque<PendingApproval>,
    status: Option<(String, bool)>,
}

impl Default for SerialWorkbench {
    fn default() -> Self {
        Self {
            open: false,
            session_id: None,
            serial_session_id: None,
            port_name: String::new(),
            ports: Vec::new(),
            settings: SerialSettings::default(),
            writable: false,
            connected_writable: false,
            opening: false,
            display_mode: SerialDisplayMode::Text,
            input_hex: false,
            line_ending: LineEnding::Cr,
            input: String::new(),
            terminal: VecDeque::new(),
            terminal_chars: 0,
            follow_output: true,
            paused: false,
            ai_prompt: String::new(),
            ai_answer: String::new(),
            ai_busy: false,
            ai_allow_readonly_queries: false,
            pending_approvals: VecDeque::new(),
            status: None,
        }
    }
}

impl SerialWorkbench {
    /// 打开并绑定到指定远程会话。
    pub fn open_for(&mut self, session_id: SessionId) -> Option<BackendCommand> {
        let close_command = self.session_id.zip(self.serial_session_id.clone()).map(
            |(old_session_id, serial_session_id)| BackendCommand::CloseSerial {
                session_id: old_session_id,
                serial_session_id,
            },
        );
        self.open = true;
        self.session_id = Some(session_id);
        self.serial_session_id = None;
        self.port_name.clear();
        self.ports.clear();
        self.writable = false;
        self.connected_writable = false;
        self.opening = false;
        self.input.clear();
        self.ai_prompt.clear();
        self.ai_busy = false;
        self.pending_approvals.clear();
        self.ai_answer.clear();
        self.ai_allow_readonly_queries = false;
        self.status = None;
        self.terminal.clear();
        self.terminal_chars = 0;
        close_command
    }

    /// 让窗口看到一项串口专属审批。
    pub fn set_approval(&mut self, approval: PendingApproval) {
        if self.session_id != Some(approval.session_id) {
            return;
        }
        if matches!(
            &approval.operation,
            RemoteOperation::OpenSerial { .. } | RemoteOperation::WriteSerial { .. }
        ) {
            if !self
                .pending_approvals
                .iter()
                .any(|item| item.approval_id == approval.approval_id)
            {
                self.pending_approvals.push_back(approval);
            }
        }
    }

    /// 合并已经在主界面处理的串口审批结果。
    pub fn resolve_approval(&mut self, approval_id: ApprovalId, approved: bool) {
        let rejected_open = !approved
            && self.pending_approvals.iter().any(|approval| {
                approval.approval_id == approval_id
                    && matches!(approval.operation, RemoteOperation::OpenSerial { .. })
            });
        self.pending_approvals
            .retain(|approval| approval.approval_id != approval_id);
        if rejected_open {
            self.opening = false;
        }
    }

    fn clear_open_approval(&mut self) {
        self.pending_approvals
            .retain(|approval| !matches!(approval.operation, RemoteOperation::OpenSerial { .. }));
    }

    fn clear_write_approval(&mut self, serial_session_id: &str) {
        self.pending_approvals.retain(|approval| {
            !matches!(
                &approval.operation,
                RemoteOperation::WriteSerial {
                    serial_session_id: approval_serial_session_id,
                    ..
                } if approval_serial_session_id == serial_session_id
            )
        });
    }

    fn clear_serial_approval_from_message(&mut self, message: &str) {
        let Some(serial_session_id) = message
            .strip_prefix("[serial:")
            .and_then(|value| value.split(']').next())
        else {
            return;
        };
        self.clear_write_approval(serial_session_id);
    }

    /// 把后端事件合并到工作台。
    pub fn apply_event(&mut self, event: &BackendEvent, translator: &Translator) {
        match event {
            BackendEvent::SerialPorts { session_id, ports }
                if Some(*session_id) == self.session_id =>
            {
                self.ports = ports.clone();
                if !self
                    .ports
                    .iter()
                    .any(|port| port.port_name == self.port_name)
                {
                    self.port_name = self
                        .ports
                        .first()
                        .map(|port| port.port_name.clone())
                        .unwrap_or_default();
                }
                self.status = None;
            }
            BackendEvent::SerialOpened {
                session_id,
                serial_session_id,
                port_name,
                writable,
            } if Some(*session_id) == self.session_id => {
                self.serial_session_id = Some(serial_session_id.clone());
                self.port_name = port_name.clone();
                self.writable = *writable;
                self.connected_writable = *writable;
                self.opening = false;
                if !*writable {
                    self.ai_allow_readonly_queries = false;
                }
                self.clear_open_approval();
                self.status = None;
                let mode = if *writable {
                    translator.text("serial.mode.writable")
                } else {
                    translator.text("serial.mode.read_only")
                };
                self.push_line(
                    TerminalSource::System,
                    translator.text_with("serial.opened", &[("port", port_name), ("mode", &mode)]),
                    &[],
                );
            }
            BackendEvent::SerialClosed {
                session_id,
                serial_session_id,
            } if Some(*session_id) == self.session_id => {
                if self.serial_session_id.as_deref() == Some(serial_session_id) {
                    self.serial_session_id = None;
                    self.writable = false;
                    self.connected_writable = false;
                    self.opening = false;
                    self.ai_busy = false;
                    self.ai_allow_readonly_queries = false;
                    self.terminal.clear();
                    self.terminal_chars = 0;
                    self.clear_write_approval(serial_session_id);
                    self.status = None;
                    self.push_line(
                        TerminalSource::System,
                        translator.text("serial.closed"),
                        &[],
                    );
                }
            }
            BackendEvent::SerialWriteCompleted {
                session_id,
                serial_session_id,
                data,
                display,
            } if Some(*session_id) == self.session_id => {
                if self.serial_session_id.as_deref() == Some(serial_session_id) {
                    self.clear_write_approval(serial_session_id);
                    self.status = None;
                    self.push_line(TerminalSource::Manual, display.clone(), data);
                }
            }
            BackendEvent::SerialStatus {
                session_id,
                message,
                error,
            } if Some(*session_id) == self.session_id => {
                if *error {
                    self.opening = false;
                }
                self.status = Some((message.clone(), *error));
            }
            BackendEvent::SerialAiAnswer {
                session_id,
                serial_session_id,
                text,
                ..
            } if Some(*session_id) == self.session_id
                && self.serial_session_id.as_deref() == Some(serial_session_id) =>
            {
                self.ai_busy = false;
                self.ai_answer = text.clone();
            }
            BackendEvent::SerialAiFailed {
                session_id,
                serial_session_id,
                message,
                ..
            } if Some(*session_id) == self.session_id
                && self.serial_session_id.as_deref() == Some(serial_session_id) =>
            {
                self.ai_busy = false;
                self.ai_answer = message.clone();
            }
            BackendEvent::RemoteEvent(event) if Some(event.session_id) == self.session_id => {
                if let remoteops_domain::EventPayload::OperationFailed { code, message } =
                    &event.payload
                    && code == "serial_read_failed"
                    && self.serial_session_id.as_deref().is_some_and(|serial_id| {
                        message.starts_with(&format!("[serial:{serial_id}]"))
                    })
                {
                    self.serial_session_id = None;
                    self.connected_writable = false;
                    self.opening = false;
                    self.ai_busy = false;
                    self.ai_allow_readonly_queries = false;
                    self.clear_serial_approval_from_message(message);
                    self.status = Some((message.clone(), true));
                }
                if let remoteops_domain::EventPayload::OutputChunk { stderr, text } = &event.payload
                {
                    if let Some(serial_id) = self.serial_session_id.as_deref() {
                        let prefix = format!("[serial:{serial_id}] ");
                        if let Some(content) = text.strip_prefix(&prefix) {
                            let (display, bytes) = serial_output(content);
                            self.push_line(
                                if *stderr {
                                    TerminalSource::DeviceError
                                } else {
                                    TerminalSource::Device
                                },
                                display,
                                &bytes,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn push_line(&mut self, source: TerminalSource, text: String, bytes: &[u8]) {
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.terminal_chars += text.chars().count();
        self.terminal.push_back(TerminalLine { source, text, hex });
        while self.terminal.len() > MAX_TERMINAL_LINES || self.terminal_chars > MAX_TERMINAL_CHARS {
            if let Some(line) = self.terminal.pop_front() {
                self.terminal_chars = self
                    .terminal_chars
                    .saturating_sub(line.text.chars().count());
            } else {
                break;
            }
        }
    }

    fn context(&self) -> String {
        let mut value = String::new();
        for line in self.terminal.iter().rev() {
            let candidate = format!("[{}] {}\n", line.source.context_label(), line.text);
            if value.chars().count() + candidate.chars().count() > MAX_AI_CONTEXT_CHARS {
                break;
            }
            value.insert_str(0, &candidate);
        }
        value
    }

    /// 渲染独立窗口；返回需要发给后端的命令。
    pub fn render(
        &mut self,
        ui: &mut Ui,
        palette: Palette,
        translator: &Translator,
    ) -> Option<BackendCommand> {
        if ui.input(|input| input.viewport().close_requested()) {
            self.open = false;
            return self.session_id.zip(self.serial_session_id.clone()).map(
                |(session_id, serial_session_id)| BackendCommand::CloseSerial {
                    session_id,
                    serial_session_id,
                },
            );
        }
        let mut command = None;
        Frame::new()
            .fill(palette.background)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                self.render_header(ui, palette, translator, &mut command);
                ui.add_space(10.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(
                            (ui.available_width() * 0.66).max(420.0),
                            ui.available_height(),
                        ),
                        Layout::top_down(Align::Min),
                        |ui| self.render_terminal(ui, palette, translator, &mut command),
                    );
                    ui.add_space(10.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| self.render_ai(ui, palette, translator, &mut command),
                    );
                });
            });
        if let Some(approval) = self.pending_approvals.front().cloned() {
            self.render_approval(ui, palette, translator, &approval, &mut command);
        }
        command
    }

    fn render_header(
        &mut self,
        ui: &mut Ui,
        palette: Palette,
        translator: &Translator,
        command: &mut Option<BackendCommand>,
    ) {
        card(
            palette,
            palette.surface,
            palette.line,
            12,
            Margin::symmetric(14, 10),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(icons::PLUGS_CONNECTED, 22.0, palette.blue));
                ui.label(
                    RichText::new(translator.text("serial.title"))
                        .size(18.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(translator.text("serial.subtitle"))
                        .size(11.0)
                        .color(palette.text_muted),
                );
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(translator.text("serial.port"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ComboBox::from_id_salt("serial_port")
                    .selected_text(if self.port_name.is_empty() {
                        translator.text("serial.select_port")
                    } else {
                        self.port_name.clone()
                    })
                    .show_ui(ui, |ui| {
                        for port in &self.ports {
                            ui.selectable_value(
                                &mut self.port_name,
                                port.port_name.clone(),
                                port.port_name.clone(),
                            );
                        }
                    });
                ui.label(
                    RichText::new(translator.text("serial.baud_rate"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ComboBox::from_id_salt("serial_baud")
                    .selected_text(self.settings.baud_rate.to_string())
                    .show_ui(ui, |ui| {
                        for baud in [9_600, 19_200, 38_400, 57_600, 115_200] {
                            ui.selectable_value(
                                &mut self.settings.baud_rate,
                                baud,
                                baud.to_string(),
                            );
                        }
                    });
                ui.label(
                    RichText::new(translator.text("serial.data_bits"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ComboBox::from_id_salt("serial_data_bits")
                    .selected_text(data_bits_label(self.settings.data_bits))
                    .show_ui(ui, |ui| {
                        for value in [
                            SerialDataBits::Five,
                            SerialDataBits::Six,
                            SerialDataBits::Seven,
                            SerialDataBits::Eight,
                        ] {
                            ui.selectable_value(
                                &mut self.settings.data_bits,
                                value,
                                data_bits_label(value),
                            );
                        }
                    });
                ui.label(
                    RichText::new(translator.text("serial.stop_bits"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ComboBox::from_id_salt("serial_stop_bits")
                    .selected_text(stop_bits_label(self.settings.stop_bits))
                    .show_ui(ui, |ui| {
                        for value in [SerialStopBits::One, SerialStopBits::Two] {
                            ui.selectable_value(
                                &mut self.settings.stop_bits,
                                value,
                                stop_bits_label(value),
                            );
                        }
                    });
                ui.label(
                    RichText::new(translator.text("serial.parity"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ComboBox::from_id_salt("serial_parity")
                    .selected_text(parity_label(self.settings.parity, translator))
                    .show_ui(ui, |ui| {
                        for value in [SerialParity::None, SerialParity::Odd, SerialParity::Even] {
                            ui.selectable_value(
                                &mut self.settings.parity,
                                value,
                                parity_label(value, translator),
                            );
                        }
                    });
                ui.label(
                    RichText::new(translator.text("serial.flow_control"))
                        .size(12.0)
                        .color(palette.text_muted),
                );
                ComboBox::from_id_salt("serial_flow")
                    .selected_text(flow_label(self.settings.flow_control, translator))
                    .show_ui(ui, |ui| {
                        for value in [
                            SerialFlowControl::None,
                            SerialFlowControl::Software,
                            SerialFlowControl::Hardware,
                        ] {
                            ui.selectable_value(
                                &mut self.settings.flow_control,
                                value,
                                flow_label(value, translator),
                            );
                        }
                    });
                ui.add_enabled_ui(self.serial_session_id.is_none() && !self.opening, |ui| {
                    ui.checkbox(&mut self.writable, translator.text("serial.writable"));
                });
                let has_session = self.session_id.is_some();
                if self.serial_session_id.is_some() {
                    let label = translator.text("serial.disconnect");
                    if icon_button(
                        ui,
                        icons::X,
                        label.as_str(),
                        ButtonKind::Ghost,
                        palette,
                        Vec2::new(100.0, 34.0),
                    )
                    .clicked()
                    {
                        if let (Some(session_id), Some(serial_session_id)) =
                            (self.session_id, self.serial_session_id.clone())
                        {
                            *command = Some(BackendCommand::CloseSerial {
                                session_id,
                                serial_session_id,
                            });
                        }
                    }
                } else {
                    let label = translator.text("serial.open");
                    if icon_button(
                        ui,
                        icons::PLUGS_CONNECTED,
                        label.as_str(),
                        ButtonKind::Primary,
                        palette,
                        Vec2::new(100.0, 34.0),
                    )
                    .clicked()
                        && has_session
                        && !self.opening
                        && !self.port_name.is_empty()
                    {
                        self.opening = true;
                        *command = Some(BackendCommand::OpenSerial {
                            session_id: self.session_id.expect("checked session"),
                            port_name: self.port_name.clone(),
                            settings: self.settings,
                            writable: self.writable,
                        });
                    }
                }
                if styled_button(
                    ui,
                    translator.text("serial.refresh"),
                    ButtonKind::Ghost,
                    palette,
                    Vec2::new(110.0, 34.0),
                )
                .clicked()
                {
                    if let Some(session_id) = self.session_id {
                        *command = Some(BackendCommand::ListSerial { session_id });
                    }
                }
            });
            if let Some((message, error)) = &self.status {
                ui.add_space(5.0);
                ui.label(RichText::new(message).size(12.0).color(if *error {
                    palette.red
                } else {
                    palette.green
                }));
            }
        });
    }

    fn render_terminal(
        &mut self,
        ui: &mut Ui,
        palette: Palette,
        translator: &Translator,
        command: &mut Option<BackendCommand>,
    ) {
        card(
            palette,
            palette.surface,
            palette.line,
            12,
            Margin::symmetric(12, 10),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(translator.text("serial.terminal"))
                        .size(15.0)
                        .strong(),
                );
                ui.checkbox(&mut self.follow_output, translator.text("serial.follow"));
                ui.checkbox(&mut self.paused, translator.text("serial.pause"));
                if styled_button(
                    ui,
                    translator.text("serial.clear"),
                    ButtonKind::Ghost,
                    palette,
                    Vec2::new(76.0, 30.0),
                )
                .clicked()
                {
                    self.terminal.clear();
                    self.terminal_chars = 0;
                }
                ui.selectable_value(
                    &mut self.display_mode,
                    SerialDisplayMode::Text,
                    translator.text("serial.display.text"),
                );
                ui.selectable_value(&mut self.display_mode, SerialDisplayMode::Hex, "HEX");
            });
            ui.add_space(6.0);
            ScrollArea::vertical()
                .stick_to_bottom(self.follow_output && !self.paused)
                .show(ui, |ui| {
                    for line in &self.terminal {
                        let source = source_label(line.source, translator);
                        let color = match line.source {
                            TerminalSource::DeviceError => palette.red,
                            TerminalSource::Manual => palette.blue,
                            TerminalSource::System | TerminalSource::Device => palette.text_muted,
                        };
                        let value = if self.display_mode == SerialDisplayMode::Hex
                            && !line.hex.is_empty()
                        {
                            &line.hex
                        } else {
                            &line.text
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("[{source}] "))
                                    .size(11.0)
                                    .color(color),
                            );
                            ui.label(RichText::new(value).size(12.0).color(palette.text));
                        });
                    }
                    if self.terminal.is_empty() {
                        ui.label(
                            RichText::new(translator.text("serial.waiting"))
                                .color(palette.text_faint),
                        );
                    }
                });
            ui.add_space(8.0);
            ui.label(
                RichText::new(translator.text("serial.manual_input"))
                    .size(13.0)
                    .strong(),
            );
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.input_hex, "HEX");
                ComboBox::from_id_salt("serial_line_ending")
                    .selected_text(self.line_ending.label(translator))
                    .show_ui(ui, |ui| {
                        for ending in [
                            LineEnding::None,
                            LineEnding::Cr,
                            LineEnding::Lf,
                            LineEnding::CrLf,
                        ] {
                            ui.selectable_value(
                                &mut self.line_ending,
                                ending,
                                ending.label(translator),
                            );
                        }
                    });
            });
            ui.add_sized(
                [ui.available_width(), 70.0],
                TextEdit::multiline(&mut self.input).hint_text(if self.input_hex {
                    translator.text("serial.input_hex_hint")
                } else {
                    translator.text("serial.input_text_hint")
                }),
            );
            if let Some((message, error)) = &self.status {
                if *error {
                    ui.label(RichText::new(message).size(12.0).color(palette.red));
                }
            }
            let send_label = translator.text("serial.send");
            if ui
                .add_enabled_ui(self.connected_writable, |ui| {
                    styled_button(
                        ui,
                        send_label,
                        ButtonKind::Primary,
                        palette,
                        Vec2::new(ui.available_width(), 36.0),
                    )
                })
                .inner
                .clicked()
            {
                match serial_input(&self.input, self.input_hex, self.line_ending) {
                    Ok(data) => {
                        if let (Some(session_id), Some(serial_session_id)) =
                            (self.session_id, self.serial_session_id.clone())
                        {
                            let display = if self.input_hex {
                                format!("HEX: {}", self.input.trim())
                            } else {
                                self.input.clone()
                            };
                            *command = Some(BackendCommand::WriteSerial {
                                session_id,
                                serial_session_id,
                                data,
                                display,
                            });
                            self.input.clear();
                        }
                    }
                    Err(error) => self.status = Some((translator.text(error.key()), true)),
                }
            }
        });
    }

    fn render_approval(
        &mut self,
        ui: &mut Ui,
        palette: Palette,
        translator: &Translator,
        approval: &PendingApproval,
        command: &mut Option<BackendCommand>,
    ) {
        let mut decision = None;
        eframe::egui::Window::new("serial_approval")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                card(
                    palette,
                    palette.surface_raised,
                    palette.line_strong,
                    12,
                    Margin::same(16),
                )
                .show(ui, |ui| {
                    ui.label(RichText::new(translator.text("serial.approval_required")).strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(&approval.reason)
                            .size(12.0)
                            .color(palette.text_muted),
                    );
                    ui.add_space(6.0);
                    Frame::new()
                        .fill(palette.surface_muted)
                        .stroke(eframe::egui::Stroke::new(1.0, palette.line))
                        .corner_radius(8.0)
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.add(
                                eframe::egui::Label::new(
                                    RichText::new(approval_details(approval, translator))
                                        .monospace()
                                        .size(11.5)
                                        .color(palette.text),
                                )
                                .wrap(),
                            );
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if styled_button(
                            ui,
                            translator.text("serial.approve"),
                            ButtonKind::Primary,
                            palette,
                            Vec2::new(100.0, 34.0),
                        )
                        .clicked()
                        {
                            decision = Some(true);
                        }
                        if styled_button(
                            ui,
                            translator.text("serial.reject"),
                            ButtonKind::Ghost,
                            palette,
                            Vec2::new(100.0, 34.0),
                        )
                        .clicked()
                        {
                            decision = Some(false);
                        }
                    });
                });
            });
        if let Some(approved) = decision {
            *command = Some(BackendCommand::DecideApproval {
                approval: Box::new(approval.clone()),
                approved,
            });
            self.resolve_approval(approval.approval_id, approved);
        }
    }

    fn render_ai(
        &mut self,
        ui: &mut Ui,
        palette: Palette,
        translator: &Translator,
        command: &mut Option<BackendCommand>,
    ) {
        card(
            palette,
            palette.surface,
            palette.line,
            12,
            Margin::symmetric(12, 10),
        )
        .show(ui, |ui| {
            ui.label(
                RichText::new(translator.text("serial.ai.title"))
                    .size(15.0)
                    .strong(),
            );
            ui.label(
                RichText::new(translator.text("serial.ai.subtitle"))
                    .size(11.0)
                    .color(palette.text_muted),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(translator.text("serial.ai.policy"))
                    .size(11.0)
                    .color(palette.text_muted),
            );
            ui.add_space(8.0);
            ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                if self.ai_answer.is_empty() {
                    ui.label(
                        RichText::new(translator.text("serial.ai.waiting"))
                            .color(palette.text_faint),
                    );
                } else {
                    ui.label(
                        RichText::new(&self.ai_answer)
                            .size(13.0)
                            .color(palette.text),
                    );
                }
            });
            ui.add_space(8.0);
            ui.add_sized(
                [ui.available_width(), 82.0],
                TextEdit::multiline(&mut self.ai_prompt)
                    .hint_text(translator.text("serial.ai.placeholder")),
            );
            ui.add_enabled_ui(self.connected_writable, |ui| {
                ui.checkbox(
                    &mut self.ai_allow_readonly_queries,
                    translator.text("serial.ai.allow_queries"),
                );
            });
            if let (Some(session_id), Some(serial_session_id)) =
                (self.session_id, self.serial_session_id.clone())
            {
                if styled_button(
                    ui,
                    translator.text("serial.ai.analyze"),
                    ButtonKind::Secondary,
                    palette,
                    Vec2::new(ui.available_width(), 38.0),
                )
                .clicked()
                    && !self.ai_busy
                    && !self.ai_prompt.trim().is_empty()
                {
                    *command = Some(BackendCommand::AskSerialAi {
                        session_id,
                        serial_session_id,
                        interaction_id: remoteops_domain::RequestId::new(),
                        prompt: self.ai_prompt.trim().to_owned(),
                        serial_context: self.context(),
                        allow_readonly_queries: self.ai_allow_readonly_queries,
                    });
                    self.ai_busy = true;
                    self.ai_allow_readonly_queries = false;
                }
            }
            if self.ai_busy {
                ui.label(
                    RichText::new(translator.text("serial.ai.busy"))
                        .size(12.0)
                        .color(palette.blue),
                );
            }
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SerialInputError {
    OddHexDigits,
    InvalidHexDigits,
    TooLarge,
}

impl SerialInputError {
    fn key(self) -> &'static str {
        match self {
            Self::OddHexDigits => "serial.hex_odd",
            Self::InvalidHexDigits => "serial.hex_invalid",
            Self::TooLarge => "serial.input_too_large",
        }
    }
}

fn serial_input(
    value: &str,
    input_hex: bool,
    ending: LineEnding,
) -> Result<Vec<u8>, SerialInputError> {
    let mut data = if input_hex {
        let compact: String = value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let bytes = compact.as_bytes();
        if !bytes.len().is_multiple_of(2) {
            return Err(SerialInputError::OddHexDigits);
        }
        if !bytes.iter().all(u8::is_ascii_hexdigit) {
            return Err(SerialInputError::InvalidHexDigits);
        }
        bytes
            .chunks_exact(2)
            .map(|pair| (hex_value(pair[0]) << 4) | hex_value(pair[1]))
            .collect()
    } else {
        value.as_bytes().to_vec()
    };
    data.extend_from_slice(ending.bytes());
    if data.len() > MAX_SERIAL_WRITE_BYTES {
        return Err(SerialInputError::TooLarge);
    }
    Ok(data)
}

fn hex_value(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => 0,
    }
}

fn serial_output(text: &str) -> (String, Vec<u8>) {
    text.strip_prefix("[base64]")
        .and_then(|encoded| BASE64.decode(encoded).ok())
        .map_or_else(
            || (text.to_owned(), text.as_bytes().to_vec()),
            |bytes| (text.to_owned(), bytes),
        )
}

fn approval_details(approval: &PendingApproval, translator: &Translator) -> String {
    match approval
        .continuation
        .as_ref()
        .map(|continuation| &continuation.completion)
    {
        Some(ApprovalCompletion::SerialOpen {
            port_name,
            writable,
        }) => {
            let settings = match &approval.operation {
                RemoteOperation::OpenSerial { settings, .. } => *settings,
                _ => SerialSettings::default(),
            };
            let baud_rate = settings.baud_rate.to_string();
            let data_bits = data_bits_label(settings.data_bits);
            let stop_bits = stop_bits_label(settings.stop_bits);
            let parity = parity_label(settings.parity, translator);
            let flow = flow_label(settings.flow_control, translator);
            let mode = if *writable {
                translator.text("serial.mode.writable")
            } else {
                translator.text("serial.mode.read_only")
            };
            translator.text_with(
                "serial.approval.open_details",
                &[
                    ("port", port_name),
                    ("baud", &baud_rate),
                    ("data_bits", data_bits),
                    ("stop_bits", stop_bits),
                    ("parity", &parity),
                    ("flow", &flow),
                    ("mode", &mode),
                ],
            )
        }
        Some(ApprovalCompletion::SerialWrite { data, display, .. }) => {
            let preview = truncate_preview(display, MAX_APPROVAL_PREVIEW_CHARS);
            let hex = data
                .iter()
                .take(MAX_APPROVAL_PREVIEW_BYTES)
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            let truncated = if data.len() > MAX_APPROVAL_PREVIEW_BYTES {
                " …"
            } else {
                ""
            };
            let hex = format!("{hex}{truncated}");
            let byte_count = data.len().to_string();
            let sha256 = match &approval.operation {
                RemoteOperation::WriteSerial { sha256, .. } => sha256.as_str(),
                _ => "-",
            };
            translator.text_with(
                "serial.approval.write_details",
                &[
                    ("preview", &preview),
                    ("hex", &hex),
                    ("bytes", &byte_count),
                    ("sha256", sha256),
                ],
            )
        }
        _ => format!("{:?}", approval.operation),
    }
}

fn truncate_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn source_label(source: TerminalSource, translator: &Translator) -> String {
    translator.text(match source {
        TerminalSource::System => "serial.source.system",
        TerminalSource::Manual => "serial.source.manual",
        TerminalSource::Device => "serial.source.device",
        TerminalSource::DeviceError => "serial.source.device_error",
    })
}

fn data_bits_label(value: SerialDataBits) -> &'static str {
    match value {
        SerialDataBits::Five => "5",
        SerialDataBits::Six => "6",
        SerialDataBits::Seven => "7",
        SerialDataBits::Eight => "8",
    }
}
fn stop_bits_label(value: SerialStopBits) -> &'static str {
    match value {
        SerialStopBits::One => "1",
        SerialStopBits::Two => "2",
    }
}
fn parity_label(value: SerialParity, translator: &Translator) -> String {
    translator.text(match value {
        SerialParity::None => "serial.parity.none",
        SerialParity::Odd => "serial.parity.odd",
        SerialParity::Even => "serial.parity.even",
    })
}
fn flow_label(value: SerialFlowControl, translator: &Translator) -> String {
    translator.text(match value {
        SerialFlowControl::None => "serial.flow.none",
        SerialFlowControl::Software => "serial.flow.software",
        SerialFlowControl::Hardware => "serial.flow.hardware",
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use remoteops_domain::{
        ApprovalId, ApprovalState, EventPayload, EventSource, RemoteEvent, RemoteOperation,
        RequestId, SessionId,
    };
    use remoteops_i18n::{Language, Translator};

    use super::{
        LineEnding, MAX_AI_CONTEXT_CHARS, MAX_SERIAL_WRITE_BYTES, SerialInputError,
        SerialWorkbench, serial_input, serial_output,
    };
    use crate::backend::{BackendCommand, BackendEvent, PendingApproval};

    #[test]
    fn context_limit_is_bounded() {
        assert_eq!(MAX_AI_CONTEXT_CHARS, 16_000);
    }

    #[test]
    fn readonly_ai_queries_are_disabled_by_default_and_on_session_change() {
        let mut workbench = SerialWorkbench::default();
        assert!(!workbench.ai_allow_readonly_queries);
        workbench.ai_allow_readonly_queries = true;
        workbench.open_for(SessionId::new());
        assert!(!workbench.ai_allow_readonly_queries);
    }

    #[test]
    fn serial_input_appends_line_ending() {
        assert_eq!(serial_input("A", false, LineEnding::Cr).unwrap(), b"A\r");
    }

    #[test]
    fn serial_input_rejects_non_ascii_hex_without_panicking() {
        assert_eq!(
            serial_input("中0", true, LineEnding::None),
            Err(SerialInputError::InvalidHexDigits)
        );
    }

    #[test]
    fn serial_input_rejects_oversized_writes() {
        assert_eq!(
            serial_input(
                &"A".repeat(MAX_SERIAL_WRITE_BYTES + 1),
                false,
                LineEnding::None,
            ),
            Err(SerialInputError::TooLarge)
        );
    }

    #[test]
    fn serial_output_decodes_agent_base64_bytes_for_hex_view() {
        let (display, bytes) = serial_output("[base64]AP8Q");
        assert_eq!(display, "[base64]AP8Q");
        assert_eq!(bytes, vec![0x00, 0xFF, 0x10]);
    }

    #[test]
    fn opening_another_target_closes_the_existing_serial_session() {
        let mut workbench = SerialWorkbench::default();
        let old_session_id = SessionId::new();
        workbench.session_id = Some(old_session_id);
        workbench.serial_session_id = Some("serial-old".to_owned());

        let command = workbench
            .open_for(SessionId::new())
            .expect("已有串口会话应先关闭");
        assert!(matches!(
            command,
            BackendCommand::CloseSerial {
                session_id,
                serial_session_id
            } if session_id == old_session_id && serial_session_id == "serial-old"
        ));
    }

    #[test]
    fn serial_approvals_are_queued_and_resolved_independently() {
        let mut workbench = SerialWorkbench::default();
        let session_id = SessionId::new();
        workbench.session_id = Some(session_id);
        let first_id = ApprovalId::new();
        for approval_id in [first_id, ApprovalId::new()] {
            workbench.set_approval(PendingApproval {
                approval_id,
                session_id,
                source: EventSource::Human,
                reason: "test".to_owned(),
                operation: RemoteOperation::WriteSerial {
                    serial_session_id: "serial-1".to_owned(),
                    byte_count: 1,
                    sha256: "00".repeat(32),
                },
                continuation: None,
            });
        }

        assert_eq!(workbench.pending_approvals.len(), 2);
        workbench.resolve_approval(first_id, false);
        assert_eq!(workbench.pending_approvals.len(), 1);
    }

    #[test]
    fn matching_serial_read_failure_disconnects_the_workbench() {
        let mut workbench = SerialWorkbench::default();
        let session_id = SessionId::new();
        workbench.session_id = Some(session_id);
        workbench.serial_session_id = Some("serial-1".to_owned());
        workbench.connected_writable = true;
        workbench.apply_event(
            &BackendEvent::RemoteEvent(RemoteEvent {
                sequence: 1,
                session_id,
                request_id: Some(RequestId::new()),
                source: EventSource::System,
                approval: ApprovalState::NotRequired,
                payload: EventPayload::OperationFailed {
                    code: "serial_read_failed".to_owned(),
                    message: "[serial:serial-1] device disconnected".to_owned(),
                },
                occurred_at: Utc::now(),
            }),
            &Translator::new(Language::EnUs),
        );

        assert!(workbench.serial_session_id.is_none());
        assert!(!workbench.connected_writable);
        assert!(workbench.status.as_ref().is_some_and(|(_, error)| *error));
    }
}
