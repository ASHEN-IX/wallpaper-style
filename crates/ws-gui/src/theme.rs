//! Visual direction for the control centre.
//!
//! The window deliberately does **not** repaint itself in the user's generated
//! palette. A settings application that recolours itself to match the wallpaper
//! makes every other window look inconsistent while it is open, and it hides the
//! very comparison the user is making. Instead the chrome stays a quiet neutral
//! slate, and the generated palette is shown in a dedicated preview panel where
//! it can be judged honestly against larger specimen surfaces.

use egui::{Color32, CornerRadius, Margin, Stroke, TextStyle, Visuals};
use ws_palette::Rgb;

/// Convert a palette colour to egui.
pub fn to_color32(color: Rgb) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

/// A preview surface: what a window would look like with this palette.
#[derive(Debug, Clone, Copy)]
pub struct PreviewColors {
    pub window: Color32,
    pub view: Color32,
    pub button: Color32,
    pub hover: Color32,
    pub selection: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub warning: Color32,
}

impl PreviewColors {
    pub fn from_palette(palette: &ws_palette::Palette) -> Self {
        Self {
            window: to_color32(palette.surface),
            view: to_color32(palette.background),
            button: to_color32(palette.button),
            hover: to_color32(palette.hover),
            selection: to_color32(palette.selection),
            text: to_color32(palette.text),
            muted: to_color32(palette.text_muted),
            accent: to_color32(palette.accent),
            accent_text: to_color32(palette.accent_text),
            warning: to_color32(palette.warning),
        }
    }
}

/// The application's own chrome colours, independent of the wallpaper.
pub struct Chrome;

impl Chrome {
    pub const BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1b);
    pub const PANEL: Color32 = Color32::from_rgb(0x1c, 0x1f, 0x25);
    pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x23, 0x27, 0x2e);
    pub const BORDER: Color32 = Color32::from_rgb(0x2e, 0x33, 0x3c);
    pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe9, 0xef);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xb2);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6b, 0x74, 0x83);
    pub const OK: Color32 = Color32::from_rgb(0x5c, 0xc9, 0x8a);
    pub const WARN: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x4e);
    pub const ERR: Color32 = Color32::from_rgb(0xe2, 0x6d, 0x6d);
    pub const ACCENT: Color32 = Color32::from_rgb(0x7c, 0x8d, 0xf0);
}

/// Install the neutral chrome style.
pub fn apply_style(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = Chrome::BG;
    visuals.window_fill = Chrome::PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(0x11, 0x13, 0x17);
    visuals.faint_bg_color = Chrome::PANEL_RAISED;
    visuals.override_text_color = Some(Chrome::TEXT);
    visuals.window_stroke = Stroke::new(1.0, Chrome::BORDER);
    visuals.selection.bg_fill = Chrome::ACCENT.gamma_multiply(0.55);
    visuals.selection.stroke = Stroke::new(1.0, Chrome::TEXT);
    visuals.hyperlink_color = Chrome::ACCENT;
    visuals.widgets.noninteractive.bg_fill = Chrome::PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Chrome::TEXT_DIM);
    visuals.widgets.inactive.bg_fill = Chrome::PANEL_RAISED;
    visuals.widgets.inactive.weak_bg_fill = Chrome::PANEL_RAISED;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Chrome::TEXT);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x2d, 0x32, 0x3c);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2d, 0x32, 0x3c);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Chrome::TEXT);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x37, 0x3d, 0x49);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Chrome::TEXT);
    visuals.widgets.open.bg_fill = Chrome::PANEL_RAISED;
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(6);
    }
    ctx.set_visuals(visuals);
    // Pin the dark theme: the chrome is designed for it and a light system theme
    // would leave the palette preview on the wrong background.
    ctx.set_theme(egui::ThemePreference::Dark);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = Margin::same(0);
    style.text_styles.insert(
        TextStyle::Heading,
        egui::FontId::new(20.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Body,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        egui::FontId::new(13.0, egui::FontFamily::Monospace),
    );
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// Draw a card container and return its inner rect for content.
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(Chrome::PANEL)
        .stroke(Stroke::new(1.0, Chrome::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// A small status pill: a dot, a label, and an optional reason in tiny text.
pub fn status_pill(ui: &mut egui::Ui, label: &str, state: PillState, reason: Option<&str>) {
    let color = match state {
        PillState::Ok => Chrome::OK,
        PillState::Warn => Chrome::WARN,
        PillState::Off => Chrome::ERR,
        PillState::Neutral => Chrome::TEXT_FAINT,
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.5, color);
        ui.label(egui::RichText::new(label).color(Chrome::TEXT).size(13.0));
        if let Some(reason) = reason {
            ui.label(
                egui::RichText::new(format!("· {reason}"))
                    .color(Chrome::TEXT_FAINT)
                    .size(12.0),
            );
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillState {
    Ok,
    Warn,
    Off,
    Neutral,
}

/// Draw a colour swatch with its hex value.
pub fn swatch(ui: &mut egui::Ui, name: &str, color: Rgb) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().min(180.0) * 0.28, 22.0),
            egui::Sense::hover(),
        );
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), to_color32(color));
        // A hairline border keeps very dark swatches visible against the card.
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Chrome::BORDER),
            egui::StrokeKind::Inside,
        );
        ui.add_space(6.0);
        ui.label(egui::RichText::new(name).color(Chrome::TEXT_DIM).size(13.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(color.to_hex_with_alpha())
                    .monospace()
                    .color(Chrome::TEXT)
                    .size(12.0),
            );
        });
    });
}

/// Render a miniature application window in the generated palette, so the user
/// judges the theme on realistic surfaces rather than a row of chips.
pub fn window_preview(ui: &mut egui::Ui, colors: &PreviewColors, show_muted: bool) {
    let width = ui.available_width().min(420.0);
    let height = 168.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();

    // Window body.
    painter.rect_filled(rect, CornerRadius::same(8), colors.window);
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, Chrome::BORDER),
        egui::StrokeKind::Inside,
    );

    // Title bar.
    let title = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 24.0));
    painter.rect_filled(
        title,
        CornerRadius {
            nw: 8,
            ne: 8,
            sw: 0,
            se: 0,
        },
        colors.button,
    );
    painter.text(
        title.left_center() + egui::vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Document — example",
        egui::FontId::proportional(11.0),
        colors.text,
    );

    // Sidebar.
    let sidebar = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 1.0, title.bottom()),
        egui::pos2(rect.left() + rect.width() * 0.30, rect.bottom() - 1.0),
    );
    painter.rect_filled(sidebar, CornerRadius::ZERO, colors.view);

    // Selected sidebar row.
    let row = egui::Rect::from_min_size(
        sidebar.min + egui::vec2(6.0, 10.0),
        egui::vec2(sidebar.width() - 12.0, 20.0),
    );
    painter.rect_filled(row, CornerRadius::same(4), colors.selection);
    painter.text(
        row.left_center() + egui::vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Selected item",
        egui::FontId::proportional(10.0),
        colors.accent_text,
    );
    // Unselected rows.
    for (index, label) in ["Another item", "Third item"].iter().enumerate() {
        let y = row.bottom() + 6.0 + index as f32 * 18.0;
        painter.text(
            egui::pos2(sidebar.left() + 12.0, y),
            egui::Align2::LEFT_CENTER,
            *label,
            egui::FontId::proportional(10.0),
            colors.text,
        );
    }

    // Content area: a heading, body text, a button and a warning line.
    let content_left = sidebar.right() + 14.0;
    painter.text(
        egui::pos2(content_left, title.bottom() + 18.0),
        egui::Align2::LEFT_CENTER,
        "Body text",
        egui::FontId::proportional(13.0),
        colors.text,
    );
    painter.text(
        egui::pos2(content_left, title.bottom() + 40.0),
        egui::Align2::LEFT_CENTER,
        "Secondary text at 4.5:1 or better",
        egui::FontId::proportional(10.0),
        if show_muted {
            colors.muted
        } else {
            colors.text
        },
    );

    let button = egui::Rect::from_min_size(
        egui::pos2(content_left, title.bottom() + 56.0),
        egui::vec2(84.0, 24.0),
    );
    painter.rect_filled(button, CornerRadius::same(5), colors.accent);
    painter.text(
        button.center(),
        egui::Align2::CENTER_CENTER,
        "Action",
        egui::FontId::proportional(10.0),
        colors.accent_text,
    );

    let hover = egui::Rect::from_min_size(
        egui::pos2(button.right() + 8.0, title.bottom() + 56.0),
        egui::vec2(84.0, 24.0),
    );
    painter.rect_filled(hover, CornerRadius::same(5), colors.hover);
    painter.text(
        hover.center(),
        egui::Align2::CENTER_CENTER,
        "Hover",
        egui::FontId::proportional(10.0),
        colors.text,
    );

    painter.text(
        egui::pos2(content_left, button.bottom() + 16.0),
        egui::Align2::LEFT_CENTER,
        "A warning message",
        egui::FontId::proportional(10.0),
        colors.warning,
    );
}

/// Section heading with a rule under it.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title)
            .color(Chrome::TEXT)
            .size(15.0)
            .strong(),
    );
    ui.add_space(2.0);
}

/// Explanatory text under a control.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .color(Chrome::TEXT_FAINT)
            .size(12.0),
    );
}
