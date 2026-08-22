//! `RemoteOps` 原生 GUI 的主题、字体和视觉令牌。

use eframe::egui::{
    Color32, Context, CursorIcon, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    TextStyle, Theme, ThemePreference, Vec2,
};
use serde::{Deserialize, Serialize};

/// 用户可选择的主题模式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ThemeMode {
    /// 跟随 Windows 系统主题。
    #[default]
    System,
    /// 强制使用浅色主题。
    Light,
    /// 强制使用深色主题。
    Dark,
}

/// V4 设计稿对应的一组颜色和阴影令牌。
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// 应用背景色。
    pub background: Color32,
    /// 主表面颜色。
    pub surface: Color32,
    /// 弹出层表面颜色。
    pub surface_raised: Color32,
    /// 弱强调表面颜色。
    pub surface_muted: Color32,
    /// 常规分隔线颜色。
    pub line: Color32,
    /// 强调分隔线颜色。
    pub line_strong: Color32,
    /// 主文字颜色。
    pub text: Color32,
    /// 次要文字颜色。
    pub text_muted: Color32,
    /// 提示文字颜色。
    pub text_faint: Color32,
    /// 主品牌蓝色。
    pub blue: Color32,
    /// 蓝色弱背景。
    pub blue_soft: Color32,
    /// 蓝色描边。
    pub blue_border: Color32,
    /// 在线和成功状态绿色。
    pub green: Color32,
    /// 成功状态弱背景。
    pub green_soft: Color32,
    /// 审批警示琥珀色。
    pub amber: Color32,
    /// 审批警示弱背景。
    pub amber_soft: Color32,
    /// 错误和拒绝红色。
    pub red: Color32,
    /// 错误状态弱背景。
    pub red_soft: Color32,
    /// 小卡片阴影。
    pub shadow_small: Shadow,
    /// 弹出层阴影。
    pub shadow_large: Shadow,
}

impl Palette {
    /// 创建浅色主题令牌。
    const fn light() -> Self {
        Self {
            background: Color32::from_rgb(245, 247, 251),
            surface: Color32::WHITE,
            surface_raised: Color32::WHITE,
            surface_muted: Color32::from_rgb(245, 247, 251),
            line: Color32::from_rgb(229, 233, 241),
            line_strong: Color32::from_rgb(215, 222, 234),
            text: Color32::from_rgb(29, 36, 51),
            text_muted: Color32::from_rgb(111, 120, 137),
            text_faint: Color32::from_rgb(154, 164, 180),
            blue: Color32::from_rgb(22, 104, 232),
            blue_soft: Color32::from_rgb(233, 242, 255),
            blue_border: Color32::from_rgb(183, 211, 255),
            green: Color32::from_rgb(30, 175, 105),
            green_soft: Color32::from_rgb(232, 248, 239),
            amber: Color32::from_rgb(232, 145, 18),
            amber_soft: Color32::from_rgb(255, 245, 220),
            red: Color32::from_rgb(214, 66, 82),
            red_soft: Color32::from_rgb(255, 240, 241),
            shadow_small: Shadow {
                offset: [0, 5],
                blur: 16,
                spread: 0,
                color: Color32::from_rgba_premultiplied(26, 40, 70, 16),
            },
            shadow_large: Shadow {
                offset: [0, 16],
                blur: 36,
                spread: 0,
                color: Color32::from_rgba_premultiplied(26, 40, 70, 38),
            },
        }
    }

    /// 创建深色主题令牌。
    const fn dark() -> Self {
        Self {
            background: Color32::from_rgb(13, 18, 27),
            surface: Color32::from_rgb(18, 25, 35),
            surface_raised: Color32::from_rgb(24, 34, 48),
            surface_muted: Color32::from_rgb(15, 23, 34),
            line: Color32::from_rgb(38, 52, 71),
            line_strong: Color32::from_rgb(52, 69, 93),
            text: Color32::from_rgb(237, 243, 255),
            text_muted: Color32::from_rgb(166, 178, 195),
            text_faint: Color32::from_rgb(113, 128, 150),
            blue: Color32::from_rgb(90, 155, 255),
            blue_soft: Color32::from_rgb(16, 40, 79),
            blue_border: Color32::from_rgb(41, 88, 156),
            green: Color32::from_rgb(53, 201, 130),
            green_soft: Color32::from_rgb(18, 58, 42),
            amber: Color32::from_rgb(246, 173, 51),
            amber_soft: Color32::from_rgb(59, 44, 22),
            red: Color32::from_rgb(255, 118, 131),
            red_soft: Color32::from_rgb(58, 31, 40),
            shadow_small: Shadow {
                offset: [0, 6],
                blur: 18,
                spread: 0,
                color: Color32::from_rgba_premultiplied(0, 0, 0, 48),
            },
            shadow_large: Shadow {
                offset: [0, 18],
                blur: 42,
                spread: 0,
                color: Color32::from_rgba_premultiplied(0, 0, 0, 92),
            },
        }
    }
}

/// 根据当前已经解析的 egui 主题返回颜色令牌。
pub fn current_palette(ctx: &Context) -> Palette {
    if ctx.theme() == Theme::Dark {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// 应用用户选择的系统、浅色或深色主题。
pub fn apply_theme(ctx: &Context, mode: ThemeMode) {
    ctx.set_theme(match mode {
        ThemeMode::System => ThemePreference::System,
        ThemeMode::Light => ThemePreference::Light,
        ThemeMode::Dark => ThemePreference::Dark,
    });
    install_design_style(ctx);
}

/// 把 V4 视觉令牌应用到 egui 的全局控件样式。
pub fn install_design_style(ctx: &Context) {
    let palette = current_palette(ctx);
    let theme = ctx.theme();
    let mut style = (*ctx.style_of(theme)).clone();
    style.animation_time = 0.16;
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.interact_size = Vec2::new(40.0, 40.0);
    style.spacing.window_margin = Margin::same(12);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );

    style.visuals.panel_fill = palette.background;
    style.visuals.window_fill = palette.surface_raised;
    style.visuals.extreme_bg_color = palette.surface_muted;
    style.visuals.faint_bg_color = palette.surface_muted;
    style.visuals.override_text_color = Some(palette.text);
    style.visuals.hyperlink_color = palette.blue;
    style.visuals.selection.bg_fill = palette.blue_soft;
    style.visuals.selection.stroke = Stroke::new(1.0, palette.blue);
    style.visuals.window_shadow = palette.shadow_large;
    style.visuals.popup_shadow = palette.shadow_large;
    style.visuals.interact_cursor = Some(CursorIcon::PointingHand);

    style.visuals.widgets.noninteractive.bg_fill = palette.surface;
    style.visuals.widgets.noninteractive.weak_bg_fill = palette.surface_muted;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.line);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text_muted);
    style.visuals.widgets.noninteractive.corner_radius = 11.0.into();

    style.visuals.widgets.inactive.bg_fill = palette.surface;
    style.visuals.widgets.inactive.weak_bg_fill = palette.surface_muted;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.line);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text_muted);
    style.visuals.widgets.inactive.corner_radius = 11.0.into();

    style.visuals.widgets.hovered.bg_fill = palette.surface_muted;
    style.visuals.widgets.hovered.weak_bg_fill = palette.blue_soft;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.5, palette.blue_border);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.hovered.corner_radius = 11.0.into();

    style.visuals.widgets.active.bg_fill = palette.blue_soft;
    style.visuals.widgets.active.weak_bg_fill = palette.blue_soft;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.5, palette.blue);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.blue);
    style.visuals.widgets.active.corner_radius = 11.0.into();
    style.visuals.widgets.open = style.visuals.widgets.active;

    ctx.set_style_of(theme, style);
}

/// 配置中文、等宽字体和 Phosphor 图标字体。
pub fn configure_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    for candidate in [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(candidate) {
            fonts.font_data.insert(
                "remoteops_zh".to_owned(),
                eframe::egui::FontData::from_owned(bytes).into(),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "remoteops_zh".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "remoteops_zh".to_owned());
            break;
        }
    }
    for candidate in [
        r"C:\Windows\Fonts\CascadiaCode.ttf",
        r"C:\Windows\Fonts\consola.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(candidate) {
            fonts.font_data.insert(
                "remoteops_mono".to_owned(),
                eframe::egui::FontData::from_owned(bytes).into(),
            );
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "remoteops_mono".to_owned());
            break;
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

