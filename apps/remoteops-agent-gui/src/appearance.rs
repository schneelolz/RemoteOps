//! Agent 界面的语义配色及可持久化外观偏好。
use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle, Vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn label_key(self) -> &'static str {
        match self {
            Self::System => "agent.preferences.system",
            Self::Light => "agent.preferences.light",
            Self::Dark => "agent.preferences.dark",
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        let (preference, native) = match self {
            Self::System => (
                egui::ThemePreference::System,
                egui::SystemTheme::SystemDefault,
            ),
            Self::Light => (egui::ThemePreference::Light, egui::SystemTheme::Light),
            Self::Dark => (egui::ThemePreference::Dark, egui::SystemTheme::Dark),
        };
        ctx.set_theme(preference);
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(native));
        ctx.request_repaint();
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub surface: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub danger_surface: Color32,
}

impl Palette {
    pub fn current(ctx: &egui::Context) -> Self {
        Self::for_theme(ctx.theme())
    }

    pub fn for_theme(theme: egui::Theme) -> Self {
        if theme == egui::Theme::Dark {
            Self {
                background: Color32::from_rgb(24, 28, 35),
                surface: Color32::from_rgb(31, 36, 44),
                text: Color32::from_rgb(242, 244, 248),
                secondary: Color32::from_rgb(174, 184, 200),
                border: Color32::from_rgb(57, 65, 77),
                accent: Color32::from_rgb(20, 105, 245),
                success: Color32::from_rgb(29, 211, 98),
                danger: Color32::from_rgb(249, 107, 119),
                danger_surface: Color32::from_rgb(46, 30, 37),
            }
        } else {
            Self {
                background: Color32::from_rgb(247, 248, 250),
                surface: Color32::from_rgb(237, 241, 246),
                text: Color32::from_rgb(24, 32, 44),
                secondary: Color32::from_rgb(86, 99, 119),
                border: Color32::from_rgb(205, 213, 224),
                accent: Color32::from_rgb(15, 103, 242),
                success: Color32::from_rgb(13, 165, 75),
                danger: Color32::from_rgb(214, 32, 58),
                danger_surface: Color32::from_rgb(255, 237, 241),
            }
        }
    }
}

pub fn install_style(ctx: &egui::Context) {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let colors = Palette::for_theme(theme);
        let mut style = (*ctx.style_of(theme)).clone();
        style.animation_time = 0.12;
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style.spacing.interact_size = Vec2::new(36.0, 34.0);
        for (kind, size) in [
            (TextStyle::Heading, 23.0),
            (TextStyle::Body, 15.0),
            (TextStyle::Button, 14.0),
        ] {
            style
                .text_styles
                .insert(kind, FontId::new(size, FontFamily::Proportional));
        }
        style.visuals.panel_fill = colors.background;
        style.visuals.window_fill = colors.surface;
        style.visuals.extreme_bg_color = colors.background;
        style.visuals.override_text_color = None;
        style.visuals.selection.bg_fill = colors.accent;
        style.visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors.text);
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            widget.corner_radius = 6.0.into();
            widget.fg_stroke = Stroke::new(1.0, colors.text);
            widget.bg_stroke = Stroke::new(1.0, colors.border);
        }
        style.visuals.widgets.inactive.bg_fill = colors.surface;
        style.visuals.widgets.inactive.weak_bg_fill = colors.surface;
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.accent);
        style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, colors.accent);
        ctx.set_style_of(theme, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_os_and_manual_preference_overrides_it() {
        let ctx = egui::Context::default();
        install_style(&ctx);
        Appearance::System.apply(&ctx);
        let _ = ctx.run_ui(
            egui::RawInput {
                system_theme: Some(egui::Theme::Dark),
                ..Default::default()
            },
            |_| {},
        );
        assert_eq!(
            Palette::current(&ctx).background,
            Palette::for_theme(egui::Theme::Dark).background
        );
        let _ = ctx.run_ui(
            egui::RawInput {
                system_theme: Some(egui::Theme::Light),
                ..Default::default()
            },
            |_| {},
        );
        assert_eq!(ctx.theme(), egui::Theme::Light);
        Appearance::Dark.apply(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        Appearance::System.apply(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Light);
    }
}
