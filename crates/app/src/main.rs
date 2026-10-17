//! GodManager: desktop entry point.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod branding;
mod details;
mod dialogs;
mod instance;
mod list;
mod platform;
mod theme;
mod toasts;
mod updates;
mod widgets;
mod window;

pub const APP_NAME: &str = "GodManager";

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| arg != instance::AFTER_UPDATE)
        .collect();
    let inbox = match instance::claim(&std::env::args().skip(1).collect::<Vec<_>>()) {
        instance::Start::First(inbox) => inbox,
        instance::Start::Forwarded => return Ok(()),
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_app_id("godmanager")
        .with_inner_size([1120.0, 720.0])
        .with_min_inner_size([760.0, 460.0])
        .with_drag_and_drop(true);
    if let Some(icon) = branding::icon_data() {
        viewport = viewport.with_icon(icon);
    }
    let remembered = window::load(egui::vec2(760.0, 460.0));
    if let Some(size) = remembered.size {
        viewport = viewport.with_inner_size(size);
    }
    viewport = viewport.with_maximized(remembered.maximized);
    let options = eframe::NativeOptions {
        viewport,
        // Kept by `window`, which never brings back the size of a minimized window.
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(app::GodApp::new(cc, args, inbox)))),
    )
}
