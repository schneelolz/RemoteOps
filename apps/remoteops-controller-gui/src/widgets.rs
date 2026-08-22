//! V4 设计稿中反复使用的原生 egui 控件。

use eframe::egui::{
    Button, Color32, CursorIcon, Frame, Margin, Response, RichText, Stroke, Ui, Vec2, WidgetText,
    style::WidgetVisuals,
};

use crate::theme::Palette;

/// `RemoteOps` 按钮的视觉类型。
#[derive(Clone, Copy, Debug)]
pub enum ButtonKind {
    /// 蓝色主操作按钮。
    Primary,
    /// 蓝色弱背景按钮。
    Secondary,
    /// 普通表面按钮。
    Ghost,
    /// 琥珀色警示按钮。
    Warning,
}

/// 创建使用 Phosphor 字体的图标文本。
pub fn icon(icon: &str, size: f32, color: Color32) -> RichText {
    RichText::new(icon).size(size).color(color)
}

/// 渲染使用统一状态反馈的原生按钮。
pub fn styled_button(
    ui: &mut Ui,
    content: impl Into<WidgetText>,
    kind: ButtonKind,
    palette: Palette,
    minimum_size: Vec2,
) -> Response {
    let (fill, hovered_fill, active_fill, border, text) = button_colors(kind, palette);
    let response = ui
        .scope(|ui| {
            let widgets = &mut ui.style_mut().visuals.widgets;
            apply_widget_visuals(&mut widgets.inactive, fill, border, text);
            apply_widget_visuals(
                &mut widgets.hovered,
                hovered_fill,
                palette.blue_border,
                text,
            );
            apply_widget_visuals(&mut widgets.active, active_fill, palette.blue, text);
            apply_widget_visuals(&mut widgets.open, active_fill, palette.blue, text);
            apply_widget_visuals(
                &mut widgets.noninteractive,
                mix_color(fill, palette.background, 0.45),
                mix_color(border, palette.background, 0.45),
                palette.text_faint,
            );
            ui.add(
                Button::new(content)
                    .corner_radius(11.0)
                    .min_size(minimum_size),
            )
        })
        .inner;
    response.on_hover_cursor(if ui.is_enabled() {
        CursorIcon::PointingHand
    } else {
        CursorIcon::NotAllowed
    })
}

/// 渲染带图标的按钮。
pub fn icon_button(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    kind: ButtonKind,
    palette: Palette,
    minimum_size: Vec2,
) -> Response {
    let (_, _, _, _, text) = button_colors(kind, palette);
    styled_button(
        ui,
        RichText::new(format!("{icon}  {label}"))
            .size(14.0)
            .strong()
            .color(text),
        kind,
        palette,
        minimum_size,
    )
}

fn button_colors(
    kind: ButtonKind,
    palette: Palette,
) -> (Color32, Color32, Color32, Color32, Color32) {
    match kind {
        ButtonKind::Primary => (
            palette.blue,
            mix_color(palette.blue, Color32::WHITE, 0.10),
            mix_color(palette.blue, Color32::BLACK, 0.16),
            palette.blue,
            Color32::WHITE,
        ),
        ButtonKind::Secondary => (
            palette.blue_soft,
            mix_color(palette.blue_soft, palette.blue, 0.18),
            mix_color(palette.blue_soft, palette.blue, 0.30),
            palette.blue_border,
            palette.blue,
        ),
        ButtonKind::Ghost => (
            palette.surface,
            mix_color(palette.surface, palette.blue, 0.08),
            mix_color(palette.surface, palette.blue, 0.16),
            palette.line,
            palette.text_muted,
        ),
        ButtonKind::Warning => (
            palette.amber_soft,
            mix_color(palette.amber_soft, palette.amber, 0.18),
            mix_color(palette.amber_soft, palette.amber, 0.30),
            palette.amber,
            palette.amber,
        ),
    }
}

fn apply_widget_visuals(
    visuals: &mut WidgetVisuals,
    fill: Color32,
    border: Color32,
    text: Color32,
) {
    visuals.bg_fill = fill;
    visuals.weak_bg_fill = fill;
    visuals.bg_stroke = Stroke::new(1.0, border);
    visuals.fg_stroke = Stroke::new(1.0, text);
    visuals.corner_radius = 11.0.into();
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn mix_color(first: Color32, second: Color32, amount: f32) -> Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8
    };
    Color32::from_rgba_unmultiplied(
        mix(first.r(), second.r()),
        mix(first.g(), second.g()),
        mix(first.b(), second.b()),
        mix(first.a(), second.a()),
    )
}

/// 创建带统一圆角、描边和阴影的卡片。
pub fn card(palette: Palette, fill: Color32, stroke: Color32, radius: u8, margin: Margin) -> Frame {
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(radius)
        .inner_margin(margin)
        .shadow(palette.shadow_small)
}
