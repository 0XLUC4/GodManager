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
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(app::GodApp::new(cc, args, inbox)))),
    )
}
