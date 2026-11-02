//! Headless rendering tests for the control centre.
//!
//! A GUI that only compiles is not evidence that it works. These tests drive the
//! real widget code through `egui::Context::run_ui`, which runs the whole layout
//! pass without an OpenGL context or a display server. That catches the failure
//! modes unit tests miss:
//!
//! * a layout that panics (bad rect arithmetic, nested scroll areas);
//! * text that cannot be measured (missing font coverage for glyphs used);
//! * a page that renders zero shapes, i.e. silently draws nothing;
//! * the palette preview producing text that fails contrast on the app's own
//!   background.
//!
//! The tests use the same `theme` helpers as the application, so restyling the
//! chrome is covered too.

#![allow(clippy::bool_assert_comparison, clippy::struct_excessive_bools)]

use eframe::egui;
use ws_gui::theme;

/// Render a screen once and return the number of painted shapes.
fn render(add: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let ctx = egui::Context::default();
    theme::apply_style(&ctx);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1120.0, 760.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, add);
    // A real renderer consumes the font-atlas deltas; a test must too, or
    // `FullOutput` panics on drop.
    output.textures_delta.clear();
    output
}

fn shape_count(output: &egui::FullOutput) -> usize {
    output
        .shapes
        .iter()
        .map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Vec(shapes) => shapes.len(),
            _ => 1,
        })
        .sum()
}

fn palette() -> ws_palette::Palette {
    ws_palette::build(
        ws_palette::Rgb::from_hex("#5f6fdd").unwrap(),
        ws_palette::Variant::Dark,
        255,
    )
    .palette
}

#[test]
fn the_style_applies_without_panicking() {
    let ctx = egui::Context::default();
    theme::apply_style(&ctx);
    assert_eq!(
        ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
        theme::Chrome::BG
    );
}

#[test]
fn a_card_with_content_renders_shapes() {
    let output = render(|ui| {
        theme::card(ui, |ui| {
            theme::section(ui, "Section");
            ui.label("Some body text");
            theme::hint(ui, "A hint line");
        });
    });
    assert!(shape_count(&output) > 0, "a card must paint something");
}

#[test]
fn status_pills_render_in_every_state() {
    for state in [
        theme::PillState::Ok,
        theme::PillState::Warn,
        theme::PillState::Off,
        theme::PillState::Neutral,
    ] {
        let output = render(|ui| {
            theme::status_pill(ui, "Feature", state, Some("because"));
            theme::status_pill(ui, "Feature", state, None);
        });
        assert!(shape_count(&output) > 0, "{state:?} painted nothing");
    }
}

#[test]
fn every_palette_colour_can_be_rendered_as_a_swatch() {
    let palette = palette();
    let output = render(|ui| {
        for role in ws_palette::PaletteRole::ALL {
            theme::swatch(ui, role.label(), role.get(&palette));
        }
    });
    assert!(shape_count(&output) > 0);
}

#[test]
fn the_window_preview_renders_for_both_variants() {
    for variant in [ws_palette::Variant::Dark, ws_palette::Variant::Light] {
        let report = ws_palette::build(ws_palette::Rgb::from_hex("#5f6fdd").unwrap(), variant, 255);
        let colors = theme::PreviewColors::from_palette(&report.palette);
        let output = render(|ui| {
            // The preview is fixed-size; give it a realistic width.
            ui.set_width(420.0);
            theme::window_preview(ui, &colors, true);
        });
        assert!(
            shape_count(&output) > 5,
            "{variant:?} preview painted too little"
        );
    }
}

#[test]
fn the_preview_text_meets_contrast_on_the_preview_surface() {
    // The preview is the user's evidence that the theme is readable. If its own
    // text failed contrast, the GUI would be lying.
    for variant in [ws_palette::Variant::Dark, ws_palette::Variant::Light] {
        let report = ws_palette::build(ws_palette::Rgb::from_hex("#5f6fdd").unwrap(), variant, 255);
        let p = &report.palette;
        for (fg, bg, what) in [
            (p.text, p.surface, "text on window"),
            (p.text, p.button, "text on title bar"),
            (p.text, p.background, "text on view"),
            (p.accent_text, p.selection, "selection text"),
            (p.accent_text, p.accent, "button text"),
            (p.text_muted, p.surface, "muted text"),
        ] {
            let ratio = fg.contrast_ratio(bg);
            let required = if what == "muted text" { 3.0 } else { 4.5 };
            assert!(
                ratio >= required,
                "{variant:?}: {what} was {ratio:.2}:1, needs {required}"
            );
        }
    }
}

#[test]
fn a_full_page_of_controls_lays_out_without_panicking() {
    // Mirrors the structure of a real page: header, two columns, scrolling list,
    // sliders and buttons. This is the shape that broke during development when
    // nested scroll areas were used carelessly.
    let mut value = 0.8_f32;
    let mut enabled = true;
    let mut check_a = true;
    let mut check_b = false;
    let mut text = String::new();
    let palette = palette();
    let colors = theme::PreviewColors::from_palette(&palette);
    let output = render(|ui| {
        egui::Panel::left("side").exact_size(208.0).show(ui, |ui| {
            for page in 0..5 {
                let _ = ui.selectable_label(page == 0, format!("Page {page}"));
            }
        });
        egui::Panel::top("bar").exact_size(46.0).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label("Status");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let _ = ui.button("Apply");
                });
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(egui::RichText::new("Page title").size(22.0).strong());
                egui::Grid::new("grid").num_columns(2).show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(380.0);
                        theme::card(ui, |ui| theme::window_preview(ui, &colors, true));
                    });
                    ui.vertical(|ui| {
                        ui.set_width(380.0);
                        theme::card(ui, |ui| {
                            ui.checkbox(&mut enabled, "Enabled");
                            ui.checkbox(&mut check_a, "Alpha");
                            ui.checkbox(&mut check_b, "Beta");
                            ui.add(egui::Slider::new(&mut value, 0.2..=1.0));
                            ui.add(egui::TextEdit::singleline(&mut text).hint_text("class"));
                            let _ = ui.button("Save");
                        });
                    });
                    ui.end_row();
                });
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for index in 0..20 {
                            theme::card(ui, |ui| {
                                ui.label(format!("Row {index}"));
                            });
                        }
                    });
            });
        });
    });
    assert!(
        shape_count(&output) > 50,
        "a full page should paint many shapes"
    );
}

#[test]
fn the_service_unit_is_pointed_at_the_cli_not_this_gui() {
    // Regression: the GUI passed its own executable to the installer, producing a
    // systemd unit that ran `wallpaper-style-gui service run` and failed on every
    // start, because `service` is a CLI subcommand.
    let cli = ws_gui::cli_binary();
    let name = cli.file_name().unwrap_or_default().to_string_lossy();
    assert_ne!(name, "wallpaper-style-gui", "the unit must not run the GUI");
    assert!(
        name == "wallpaper-style" || name == "wallpaper-style-rs",
        "expected a CLI binary name, got {name}"
    );
}

#[test]
fn the_default_style_is_dark_and_readable() {
    let ctx = egui::Context::default();
    theme::apply_style(&ctx);
    let visuals = ctx.style_of(egui::Theme::Dark).visuals.clone();
    let to_rgb = |c: egui::Color32| ws_palette::Rgb::rgb(c.r(), c.g(), c.b());
    let ratio =
        to_rgb(visuals.override_text_color.unwrap()).contrast_ratio(to_rgb(visuals.panel_fill));
    assert!(
        ratio >= 4.5,
        "chrome text was {ratio:.2}:1 against the panel"
    );
}
