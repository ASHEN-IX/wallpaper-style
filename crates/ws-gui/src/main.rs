//! Wallpaper Style control centre.
//!
//! A native window, not a web view: the application reads and writes desktop
//! configuration files and must work on a plain Plasma 6 session without a
//! browser engine. `eframe`/`egui` renders with OpenGL through GLX/EGL and
//! speaks Wayland directly.

use ws_gui::{app, theme};

use eframe::egui;

fn main() -> eframe::Result<()> {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned());
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter))
        .try_init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Wallpaper Style")
            .with_app_id("wallpaper-style")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([900.0, 620.0])
            .with_icon(window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Wallpaper Style",
        options,
        Box::new(|cc| match app::WallpaperStyleApp::new(cc) {
            Ok(app) => Ok(Box::new(app) as Box<dyn eframe::App>),
            Err(err) => {
                // Failing to start is a supported outcome (unsupported desktop);
                // show it rather than exiting silently.
                eprintln!("wallpaper-style-gui: {err}");
                Ok(Box::new(StartupError::new(err)) as Box<dyn eframe::App>)
            }
        }),
    )
}

/// A window that explains why the application could not start.
struct StartupError {
    message: String,
}

impl StartupError {
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl eframe::App for StartupError {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        theme::apply_style(root.ctx());
        egui::CentralPanel::default().show(root, |ui| {
            ui.add_space(24.0);
            ui.label(
                egui::RichText::new("Wallpaper Style could not start")
                    .size(20.0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&self.message).color(theme::Chrome::ERR));
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(
                    "Run `wallpaper-style doctor` in a terminal for a full diagnosis.",
                )
                .color(theme::Chrome::TEXT_DIM),
            );
        });
    }
}

/// Generate a small icon at runtime rather than shipping a binary asset, so the
/// window has a recognisable icon without a build-time image pipeline.
fn window_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = ((y * SIZE + x) * 4) as usize;
            // A rounded square with a diagonal gradient in the app's accent,
            // fading towards the bottom right.
            let margin = 6.0_f32;
            let inside = x as f32 >= margin
                && y as f32 >= margin
                && (x as f32) < (SIZE as f32 - margin)
                && (y as f32) < (SIZE as f32 - margin);
            if !inside {
                continue;
            }
            let t = (x as f32 + y as f32) / (2.0 * SIZE as f32);
            rgba[index] = (0x50 as f32 + 0x5c as f32 * t) as u8;
            rgba[index + 1] = (0x62 as f32 + 0x45 as f32 * (1.0 - t)) as u8;
            rgba[index + 2] = (0xd8 as f32 + 0x18 as f32 * (1.0 - t)) as u8;
            rgba[index + 3] = 255;
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}
