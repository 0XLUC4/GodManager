//! Modal windows: add downloads, settings, confirm a removal.

use std::path::PathBuf;

use egui::{
    Button, Context, CornerRadius, DragValue, Id, Modal, RichText, Slider, TextEdit, Ui, vec2,
};
use godmanager_core::manager::Settings;

use crate::platform;
use crate::theme::Palette;
use crate::widgets::{FIELD_MARGIN, icon, with_icon};

/// Black (white in dark mode) button for the main choice of a window.
fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    let palette = Palette::of(ui);
    ui.add(
        Button::new(RichText::new(label).color(palette.bg).strong())
            .fill(palette.accent)
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(96.0, 30.0)),
    )
}

fn title(ui: &mut Ui, glyph: &str, text: &str) {
    ui.label(RichText::new(with_icon(glyph, text)).size(18.0).strong());
    ui.add_space(8.0);
}

fn folder_row(ui: &mut Ui, folder: &mut PathBuf) {
    ui.horizontal(|ui| {
        let palette = Palette::of(ui);
        let width = (ui.available_width() - 100.0).max(120.0);
        let shown =
            crate::list::one_line(ui, &folder.display().to_string(), 13.5, palette.text, width);
        ui.label(shown).on_hover_text(folder.display().to_string());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(with_icon(icon::FOLDER, "Change…")).clicked()
                && let Some(picked) = platform::pick_folder(folder)
            {
                *folder = picked;
            }
        });
    });
}

pub enum AddOutcome {
    Open,
    Cancel,
    Submit,
    PickTorrents,
}

pub struct AddDialog {
    pub links: String,
    pub folder: PathBuf,
    pub start: bool,
    pub connections: usize,
    focus: bool,
}

impl AddDialog {
    pub fn new(links: String, settings: &Settings) -> Self {
        Self {
            links,
            folder: settings.folder.clone(),
            start: true,
            connections: settings.connections,
            focus: true,
        }
    }

    pub fn show(&mut self, ctx: &Context) -> AddOutcome {
        let mut outcome = AddOutcome::Open;
        let response = Modal::new(Id::new("add-downloads")).show(ctx, |ui| {
            ui.set_width(540.0);
            title(ui, icon::PLUS, "Add downloads");
            let palette = Palette::of(ui);
            ui.label(
                RichText::new("Web addresses and magnet links, one per line")
                    .color(palette.text_dim),
            );
            let field = ui.add(
                TextEdit::multiline(&mut self.links)
                    .hint_text("https://…   or   magnet:?xt=urn:btih:…")
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .margin(FIELD_MARGIN),
            );
            if std::mem::take(&mut self.focus) {
                field.request_focus();
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Save to").color(palette.text_dim));
            folder_row(ui, &mut self.folder);
            let has_web = self
                .links
                .lines()
                .any(|line| line.trim().starts_with("http"));
            if has_web {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Connections per file").color(palette.text_dim));
                    ui.add(Slider::new(&mut self.connections, 1..=16));
                });
            }
            ui.checkbox(&mut self.start, "Start now");
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .button(with_icon(icon::MAGNET, "Torrent file…"))
                    .clicked()
                {
                    outcome = AddOutcome::PickTorrents;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ready = !self.links.trim().is_empty();
                    let submit = ui
                        .add_enabled_ui(ready, |ui| primary_button(ui, "Download"))
                        .inner;
                    let ctrl_enter = ui.input(|input| {
                        input.modifiers.command && input.key_pressed(egui::Key::Enter)
                    });
                    if ready && (submit.clicked() || ctrl_enter) {
                        outcome = AddOutcome::Submit;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = AddOutcome::Cancel;
                    }
                });
            });
        });
        if response.should_close() && matches!(outcome, AddOutcome::Open) {
            outcome = AddOutcome::Cancel;
        }
        outcome
    }
}

pub enum SettingsOutcome {
    Open,
    Cancel,
    Save,
}

pub struct SettingsDialog {
    pub draft: Settings,
    /// Result of the last "open links with GodManager".
    associated: Option<Result<(), String>>,
}

impl SettingsDialog {
    pub fn new(settings: &Settings) -> Self {
        Self {
            draft: settings.clone(),
            associated: None,
        }
    }

    pub fn show(&mut self, ctx: &Context) -> SettingsOutcome {
        let mut outcome = SettingsOutcome::Open;
        let response = Modal::new(Id::new("settings")).show(ctx, |ui| {
            ui.set_width(500.0);
            title(ui, icon::GEAR_SIX, "Settings");
            let palette = Palette::of(ui);
            let dim = |text: &str| RichText::new(text.to_owned()).color(palette.text_dim);
            let draft = &mut self.draft;
            ui.label(dim("Downloads go to"));
            folder_row(ui, &mut draft.folder);
            ui.add_space(8.0);
            egui::Grid::new("settings-grid")
                .num_columns(2)
                .spacing(vec2(16.0, 8.0))
                .show(ui, |ui| {
                    ui.label(dim("Connections per file"));
                    ui.add(Slider::new(&mut draft.connections, 1..=16));
                    ui.end_row();
                    ui.label(dim("Files at the same time"));
                    ui.add(Slider::new(&mut draft.parallel, 1..=10));
                    ui.end_row();
                    ui.label(dim("Download limit"));
                    limit(ui, &mut draft.down_limit_kb);
                    ui.end_row();
                    ui.label(dim("Upload limit (torrents)"));
                    limit(ui, &mut draft.up_limit_kb);
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.checkbox(&mut draft.seed, "Keep sharing torrents once they are done");
            if cfg!(windows) {
                ui.add_space(10.0);
                let label = with_icon(
                    icon::MAGNET,
                    "Open magnet links and .torrent files with GodManager",
                );
                if ui.button(label).clicked() {
                    self.associated = Some(platform::associate());
                }
                match &self.associated {
                    Some(Ok(())) => {
                        ui.label(dim("Done. Your browser may ask once to open GodManager."));
                    }
                    Some(Err(err)) => {
                        ui.label(RichText::new(err).color(palette.error));
                    }
                    None => {}
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .link(dim("Data folder"))
                    .on_hover_text(platform::data_dir().display().to_string())
                    .clicked()
                {
                    let _ = platform::open(&platform::data_dir());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, "Save").clicked() {
                        outcome = SettingsOutcome::Save;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = SettingsOutcome::Cancel;
                    }
                });
            });
        });
        if response.should_close() && matches!(outcome, SettingsOutcome::Open) {
            outcome = SettingsOutcome::Cancel;
        }
        outcome
    }
}

/// KB/s with 0 shown as "No limit".
fn limit(ui: &mut Ui, value: &mut u64) {
    ui.horizontal(|ui| {
        ui.add(
            DragValue::new(value)
                .range(0..=1_000_000)
                .speed(16)
                .custom_formatter(|value, _| {
                    if value == 0.0 {
                        "No limit".to_owned()
                    } else {
                        format!("{value:.0} KB/s")
                    }
                })
                .custom_parser(|text| {
                    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                    Some(digits.parse::<f64>().unwrap_or(0.0))
                }),
        );
        if *value > 0 && ui.small_button("No limit").clicked() {
            *value = 0;
        }
    });
}

pub enum RemoveOutcome {
    Open,
    Cancel,
    Remove { delete_files: bool },
}

pub struct RemoveDialog {
    pub id: u64,
    pub name: String,
    pub delete_files: bool,
}

impl RemoveDialog {
    pub fn show(&mut self, ctx: &Context) -> RemoveOutcome {
        let mut outcome = RemoveOutcome::Open;
        let response = Modal::new(Id::new("remove")).show(ctx, |ui| {
            ui.set_width(400.0);
            title(ui, icon::TRASH, "Remove the download");
            ui.label(format!("“{}” leaves the list.", self.name));
            ui.add_space(6.0);
            ui.checkbox(&mut self.delete_files, "Also delete the downloaded files");
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary_button(ui, "Remove").clicked() {
                    outcome = RemoveOutcome::Remove {
                        delete_files: self.delete_files,
                    };
                }
                if ui.button("Cancel").clicked() {
                    outcome = RemoveOutcome::Cancel;
                }
            });
        });
        if response.should_close() && matches!(outcome, RemoveOutcome::Open) {
            outcome = RemoveOutcome::Cancel;
        }
        outcome
    }
}
