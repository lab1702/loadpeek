//! Catppuccin Mocha with deliberately restricted text/background pairings.
//!
//! Text and data colors belong on BASE, MANTLE, CRUST, or SURFACE0. SURFACE1
//! is a decorative border/grid color, not a background for secondary text.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle};

pub const BASE: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x2e);
pub const MANTLE: Color32 = Color32::from_rgb(0x18, 0x18, 0x25);
pub const CRUST: Color32 = Color32::from_rgb(0x11, 0x11, 0x1b);
pub const SURFACE0: Color32 = Color32::from_rgb(0x31, 0x32, 0x44);
pub const SURFACE1: Color32 = Color32::from_rgb(0x45, 0x47, 0x5a);
pub const TEXT: Color32 = Color32::from_rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT: Color32 = Color32::from_rgb(0xa6, 0xad, 0xc8);
pub const MAUVE: Color32 = Color32::from_rgb(0xcb, 0xa6, 0xf7);
pub const BLUE: Color32 = Color32::from_rgb(0x89, 0xb4, 0xfa);
pub const GREEN: Color32 = Color32::from_rgb(0xa6, 0xe3, 0xa1);
pub const TEAL: Color32 = Color32::from_rgb(0x94, 0xe2, 0xd5);
pub const PEACH: Color32 = Color32::from_rgb(0xfa, 0xb3, 0x87);
pub const YELLOW: Color32 = Color32::from_rgb(0xf9, 0xe2, 0xaf);
pub const RED: Color32 = Color32::from_rgb(0xf3, 0x8b, 0xa8);
pub const LAVENDER: Color32 = Color32::from_rgb(0xb4, 0xbe, 0xfe);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(25.0));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(14.0));
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size = egui::vec2(36.0, 36.0);
        style.animation_time = 0.0;

        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(TEXT);
        visuals.weak_text_color = Some(SUBTEXT);
        visuals.panel_fill = BASE;
        visuals.window_fill = MANTLE;
        visuals.extreme_bg_color = CRUST;
        visuals.faint_bg_color = MANTLE;
        visuals.code_bg_color = SURFACE0;
        visuals.text_edit_bg_color = Some(BASE);
        visuals.hyperlink_color = BLUE;
        visuals.warn_fg_color = YELLOW;
        visuals.error_fg_color = RED;
        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.menu_corner_radius = CornerRadius::same(8);
        visuals.window_stroke = Stroke::new(1.0, SURFACE1);
        visuals.selection.bg_fill = SURFACE0;
        visuals.selection.stroke = Stroke::new(2.0, MAUVE);
        visuals.slider_trailing_fill = true;

        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.bg_fill = SURFACE0;
            widget.weak_bg_fill = SURFACE0;
            widget.bg_stroke = Stroke::new(1.0, SUBTEXT);
            widget.fg_stroke = Stroke::new(1.5, TEXT);
            widget.corner_radius = CornerRadius::same(7);
            widget.expansion = 0.0;
        }
        visuals.widgets.noninteractive.bg_fill = MANTLE;
        visuals.widgets.noninteractive.weak_bg_fill = MANTLE;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, SURFACE1);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.5, LAVENDER);
        // egui uses the active style for keyboard focus as well as presses.
        visuals.widgets.active.bg_stroke = Stroke::new(2.0, MAUVE);
        visuals.widgets.open.bg_stroke = Stroke::new(2.0, MAUVE);
        style.visuals = visuals;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color32) -> f64 {
        fn linear(channel: u8) -> f64 {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn all_text_colors_pass_wcag_aa_on_text_surfaces() {
        for background in [BASE, MANTLE, CRUST, SURFACE0] {
            for foreground in [
                TEXT, SUBTEXT, MAUVE, BLUE, GREEN, TEAL, PEACH, YELLOW, RED, LAVENDER,
            ] {
                let ratio = contrast(foreground, background);
                assert!(
                    ratio >= 4.5,
                    "{foreground:?} on {background:?}: {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn chart_strokes_and_control_boundaries_pass_nontext_contrast() {
        for foreground in [MAUVE, BLUE, GREEN, TEAL, PEACH, YELLOW, RED, LAVENDER] {
            assert!(contrast(foreground, BASE) >= 3.0);
        }
        for background in [BASE, MANTLE, CRUST, SURFACE0] {
            for boundary in [SUBTEXT, MAUVE, LAVENDER] {
                assert!(contrast(boundary, background) >= 3.0);
            }
        }
    }
}
