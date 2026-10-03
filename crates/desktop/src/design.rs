//! Atari Console Pro: semantic colors and reusable, accessible egui components.
use crate::preferences::Preferences;
use eframe::egui::{self, Color32, RichText, Stroke};

pub const AMBER: Color32 = Color32::from_rgb(255, 176, 0);
pub const RED: Color32 = Color32::from_rgb(255, 111, 122);
pub const GREEN: Color32 = Color32::from_rgb(94, 235, 150);
pub const CYAN: Color32 = Color32::from_rgb(89, 209, 239);
pub const BG: Color32 = Color32::from_rgb(11, 15, 26);
pub const SURFACE: Color32 = Color32::from_rgb(17, 27, 43);
pub const TEXT: Color32 = Color32::from_rgb(214, 236, 255);
pub const DIM: Color32 = Color32::from_rgb(154, 175, 199);
pub const FRAME: Color32 = Color32::from_rgb(47, 73, 108);
pub const SELECTED: Color32 = Color32::from_rgb(24, 53, 85);

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "console".into(),
        egui::FontData::from_static(include_bytes!(
            "../assets/fonts/JetBrainsMonoNL-Regular.ttf"
        ))
        .into(),
    );
    fonts.font_data.insert(
        "console-medium".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/JetBrainsMonoNL-Medium.ttf"))
            .into(),
    );
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts
            .families
            .get_mut(&family)
            .unwrap()
            .insert(0, "console".into());
    }
    fonts.families.insert(
        egui::FontFamily::Name("medium".into()),
        vec!["console-medium".into(), "console".into()],
    );
    ctx.set_fonts(fonts);
}
pub fn apply(ctx: &egui::Context, prefs: &Preferences) {
    ctx.set_theme(match prefs.dark {
        Some(true) => egui::ThemePreference::Dark,
        Some(false) => egui::ThemePreference::Light,
        None => egui::ThemePreference::System,
    });
    ctx.set_zoom_factor(prefs.scale);
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let dark = theme == egui::Theme::Dark;
        ctx.style_mut_of(theme, |style| {
            let size = if prefs.compact { 13.0 } else { 14.0 };
            for (kind, size) in [
                (egui::TextStyle::Body, size),
                (egui::TextStyle::Button, size),
                (egui::TextStyle::Monospace, size),
                (egui::TextStyle::Small, 12.0),
                (egui::TextStyle::Heading, 20.0),
            ] {
                style
                    .text_styles
                    .insert(kind, egui::FontId::monospace(size));
            }
            style.spacing.item_spacing = egui::vec2(10.0, if prefs.compact { 4.0 } else { 6.0 });
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size = egui::vec2(28.0, if prefs.compact { 28.0 } else { 32.0 });
            style.spacing.window_margin = egui::Margin::same(20);
            style.animation_time = if prefs.reduce_motion || crate::native::reduce_motion() {
                0.0
            } else {
                0.12
            };
            let mut v = if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };
            let text = if dark {
                TEXT
            } else {
                Color32::from_rgb(23, 40, 63)
            };
            let surface = if dark {
                SURFACE
            } else {
                Color32::from_rgb(237, 243, 250)
            };
            let frame = if dark {
                FRAME
            } else {
                Color32::from_rgb(161, 181, 207)
            };
            let accent = if dark {
                AMBER
            } else {
                Color32::from_rgb(137, 83, 0)
            };
            v.override_text_color = Some(text);
            v.weak_text_color = Some(if dark {
                DIM
            } else {
                Color32::from_rgb(75, 95, 118)
            });
            v.panel_fill = if dark {
                BG
            } else {
                Color32::from_rgb(249, 251, 254)
            };
            v.window_fill = surface;
            v.window_stroke = Stroke::new(1.0, frame);
            v.window_corner_radius = egui::CornerRadius::same(4);
            v.menu_corner_radius = egui::CornerRadius::same(3);
            v.extreme_bg_color = if dark {
                Color32::from_rgb(8, 13, 22)
            } else {
                Color32::WHITE
            };
            v.faint_bg_color = surface;
            v.code_bg_color = surface;
            v.selection.bg_fill = if dark {
                SELECTED
            } else {
                Color32::from_rgb(207, 224, 244)
            };
            v.selection.stroke = Stroke::new(1.5, text);
            v.hyperlink_color = if dark {
                CYAN
            } else {
                Color32::from_rgb(0, 96, 132)
            };
            v.warn_fg_color = accent;
            v.error_fg_color = if dark {
                RED
            } else {
                Color32::from_rgb(176, 36, 51)
            };
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = egui::CornerRadius::same(3);
                w.expansion = 0.0;
                w.fg_stroke = Stroke::new(1.0, text);
                w.bg_stroke = Stroke::new(1.0, frame);
                w.bg_fill = surface;
                w.weak_bg_fill = surface;
            }
            v.widgets.hovered.bg_fill = v.selection.bg_fill;
            v.widgets.hovered.weak_bg_fill = v.selection.bg_fill;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
            v.widgets.active.bg_stroke = Stroke::new(1.5, v.hyperlink_color);
            v.widgets.active.bg_fill = v.selection.bg_fill;
            v.widgets.active.weak_bg_fill = v.selection.bg_fill;
            v.widgets.open = v.widgets.active;
            style.visuals = v;
        });
    }
}
pub fn row_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y + 2.0
}
pub fn accent(ui: &egui::Ui) -> Color32 {
    ui.visuals().warn_fg_color
}
pub fn muted(ui: &egui::Ui) -> Color32 {
    ui.visuals().weak_text_color()
}
pub fn semantic(ui: &egui::Ui, color: Color32) -> Color32 {
    if ui.visuals().dark_mode {
        color
    } else if color == GREEN {
        Color32::from_rgb(19, 109, 64)
    } else if color == CYAN {
        Color32::from_rgb(0, 96, 132)
    } else if color == RED {
        ui.visuals().error_fg_color
    } else if color == AMBER {
        accent(ui)
    } else {
        color
    }
}
pub fn label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(text.into()).truncate().selectable(true))
}

/// Highlight the server address, leaving credentials and the whole command selectable.
pub fn copy_command(ui: &mut egui::Ui, command: &str) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    let font = ui.style().text_styles[&egui::TextStyle::Monospace].clone();
    let normal = egui::TextFormat {
        font_id: font.clone(),
        color: ui.visuals().text_color(),
        ..Default::default()
    };
    if let Some(range) = copy_address_range(command) {
        job.append(&command[..range.start], 0.0, normal.clone());
        job.append(
            &command[range.clone()],
            0.0,
            egui::TextFormat {
                font_id: font,
                color: semantic(ui, AMBER),
                background: ui.visuals().selection.bg_fill,
                ..Default::default()
            },
        );
        job.append(&command[range.end..], 0.0, normal);
    } else {
        job.append(command, 0.0, normal);
    }
    ui.add(egui::Label::new(job).wrap().selectable(true))
}
pub fn copy_address_range(command: &str) -> Option<std::ops::Range<usize>> {
    let begin = command.find("://")? + 3;
    let end = command[begin..]
        .find(['/', ' '])
        .map(|i| begin + i)
        .unwrap_or(command.len());
    let start = command[begin..end]
        .rfind('@')
        .map(|i| begin + i + 1)
        .unwrap_or(begin);
    let authority = &command[start..end];
    if authority.starts_with('[') {
        let close = authority.find(']')?;
        Some(start + 1..start + close)
    } else {
        Some(start..start + authority.find(':').unwrap_or(authority.len()))
    }
}

pub fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(egui::FontId::new(
                    14.0,
                    egui::FontFamily::Name("medium".into()),
                ))
                .color(accent(ui)),
        );
        ui.label(RichText::new(subtitle).small().color(muted(ui)));
    });
}
pub fn panel<R>(
    ui: &mut egui::Ui,
    title: &str,
    focused: bool,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let frame = egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(
            if focused { 1.5 } else { 1.0 },
            if focused {
                ui.visuals().hyperlink_color
            } else {
                ui.visuals().window_stroke.color
            },
        ))
        .corner_radius(3)
        .inner_margin(12);
    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width().max(0.0));
        heading(ui, title, "");
        ui.add_space(4.0);
        contents(ui)
    })
}
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let color = semantic(ui, color);
    egui::Frame::new()
        .fill(color.gamma_multiply(if ui.visuals().dark_mode { 0.12 } else { 0.08 }))
        .stroke(Stroke::new(0.5, color.gamma_multiply(0.5)))
        .corner_radius(3)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)))
        .inner
}
pub fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    if !ui.is_enabled() {
        return ui.button(text);
    }
    ui.add(
        egui::Button::new(RichText::new(text).color(if ui.visuals().dark_mode {
            BG
        } else {
            Color32::WHITE
        }))
        .fill(accent(ui)),
    )
}
pub fn empty(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.add_space(18.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).color(muted(ui)));
        ui.add_space(6.0);
        ui.label(RichText::new(detail).small().color(muted(ui)));
    });
    ui.add_space(18.0);
}
pub fn progress(ui: &mut egui::Ui, fraction: Option<f64>, color: Color32) {
    // Standard accessible progress widget; segmented track adds the console character.
    let p = fraction.unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    let r = ui.add(
        egui::ProgressBar::new(p)
            .desired_width(ui.available_width().min(240.0))
            .desired_height(20.0)
            .corner_radius(2)
            .fill(if fraction.is_some() {
                semantic(ui, color)
            } else {
                ui.visuals().faint_bg_color
            })
            .text(
                RichText::new(
                    fraction
                        .map(|_| format!("{:.0}%", p * 100.0))
                        .unwrap_or_else(|| "Waiting".into()),
                )
                .color(if p > 0.55 {
                    if ui.visuals().dark_mode {
                        BG
                    } else {
                        Color32::WHITE
                    }
                } else {
                    ui.visuals().text_color()
                }),
            ),
    );
    for i in 1..20 {
        let x = r.rect.left() + r.rect.width() * i as f32 / 20.0;
        ui.painter().vline(
            x,
            (r.rect.bottom() - 3.0)..=r.rect.bottom(),
            Stroke::new(1.0, ui.visuals().panel_fill),
        );
    }
}
pub fn key(ui: &mut egui::Ui, key: &str, description: &str) {
    ui.label(RichText::new(key).small().color(accent(ui)));
    ui.label(RichText::new(description).small().color(muted(ui)));
}
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut pixels = vec![0; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 / size as f32;
            let fy = y as f32 / size as f32;
            let bracket = ((0.18..0.24).contains(&fx) || (0.76..0.82).contains(&fx))
                && (0.22..0.78).contains(&fy)
                || ((0.18..0.34).contains(&fx) || (0.66..0.82).contains(&fx))
                    && ((0.22..0.28).contains(&fy) || (0.72..0.78).contains(&fy));
            let arrow = (0.36..0.65).contains(&fx)
                && ((0.37..0.42).contains(&fy) || (0.58..0.63).contains(&fy))
                || ((fx - 0.61).abs() + (fy - 0.395).abs() < 0.10 && fx > 0.56)
                || ((fx - 0.39).abs() + (fy - 0.605).abs() < 0.10 && fx < 0.44);
            let rgba = if bracket {
                [255, 176, 0, 255]
            } else if arrow {
                [89, 209, 239, 255]
            } else {
                [0, 0, 0, 0]
            };
            let index = ((y * size + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&rgba);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_highlights_only_the_address_and_keeps_url_credentials() {
        for (command, address) in [
            (
                "copy ftp://cisco:cisco123@10.10.100.23:2121/image.bin flash:",
                "10.10.100.23",
            ),
            (
                "copy https://[2001:db8::1]:8443/image.bin flash:",
                "2001:db8::1",
            ),
            ("copy sftp://cisco@10.1.2.3/image.bin flash:", "10.1.2.3"),
        ] {
            assert_eq!(&command[copy_address_range(command).unwrap()], address);
        }
        assert!(copy_address_range("show version").is_none());
    }
}
