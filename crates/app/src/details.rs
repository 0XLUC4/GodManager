//! Pane under the list: everything about the selected download.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::{CornerRadius, Grid, Rect, RichText, ScrollArea, Sense, Ui, vec2};
use godmanager_core::format;
use godmanager_core::http::UNKNOWN_END;
use godmanager_core::manager::{Row, Status};

use crate::app::Action;
use crate::theme::Palette;
use crate::widgets::{self, icon, with_icon};

/// SHA-256 of a finished file, computed in the background.
pub struct Checksum {
    pub id: u64,
    result: Arc<Mutex<Option<Result<String, String>>>>,
    /// What the site says the hash is, pasted by the user.
    pub expected: String,
}

impl Checksum {
    pub fn start(ctx: &egui::Context, id: u64, path: PathBuf) -> Self {
        let result = Arc::new(Mutex::new(None));
        let slot = result.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("sha256".into())
            .spawn(move || {
                let hash = godmanager_core::checksum::sha256(&path);
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(hash);
                }
                ctx.request_repaint();
            });
        if let Err(err) = spawned
            && let Ok(mut slot) = result.lock()
        {
            *slot = Some(Err(format!("Cannot start: {err}")));
        }
        Self {
            id,
            result,
            expected: String::new(),
        }
    }

    fn result(&self) -> Option<Result<String, String>> {
        self.result.lock().ok().and_then(|slot| slot.clone())
    }
}

pub fn show(
    ui: &mut Ui,
    palette: &Palette,
    row: &Row,
    checksum: &mut Option<Checksum>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::list::kind_glyph(row)).size(18.0));
        let name = crate::list::one_line(
            ui,
            &row.name,
            16.0,
            palette.text,
            ui.available_width() - 40.0,
        );
        ui.label(name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::small_icon_button(ui, icon::X, "Close the details").clicked() {
                actions.push(Action::CloseDetails);
            }
        });
    });
    ui.add_space(4.0);
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Grid::new("details-grid")
            .num_columns(2)
            .spacing(vec2(18.0, 6.0))
            .show(ui, |ui| {
                let dim = |text: &str| RichText::new(text.to_owned()).color(palette.text_dim);
                ui.label(dim("Status"));
                let status = match &row.status {
                    Status::Failed(err) => format!("Failed: {err}"),
                    status => status.label().to_owned(),
                };
                ui.label(status);
                ui.end_row();

                ui.label(dim("Size"));
                ui.label(match row.total {
                    Some(total) => format!("{} of {}", format::size(row.done), format::size(total)),
                    None => format::size(row.done),
                });
                ui.end_row();

                ui.label(dim(if row.torrent {
                    "Magnet link"
                } else {
                    "Address"
                }));
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 40.0).max(80.0);
                    let link = crate::list::one_line(ui, &row.link, 13.0, palette.text, width);
                    ui.label(link).on_hover_text(&row.link);
                    if widgets::small_icon_button(ui, icon::COPY, "Copy").clicked() {
                        actions.push(Action::CopyLink(row.id));
                    }
                });
                ui.end_row();

                ui.label(dim("Saved to"));
                let place = row.path.as_ref().unwrap_or(&row.folder);
                if ui
                    .link(place.display().to_string())
                    .on_hover_text("Show in folder")
                    .clicked()
                {
                    actions.push(Action::Reveal(row.id));
                }
                ui.end_row();

                if row.torrent {
                    ui.label(dim("Peers"));
                    ui.label(row.peers.to_string());
                    ui.end_row();
                    ui.label(dim("Sent"));
                    let ratio = if row.done > 0 {
                        format!("  (ratio {:.2})", row.uploaded as f64 / row.done as f64)
                    } else {
                        String::new()
                    };
                    ui.label(format!("{}{ratio}", format::size(row.uploaded)));
                    ui.end_row();
                } else if !matches!(row.status, Status::Finished) {
                    ui.label(dim("Pause"));
                    ui.label(if row.resumable {
                        "Can pause and go on later"
                    } else {
                        "This server cannot resume: a pause starts over"
                    });
                    ui.end_row();
                }
            });

        if !row.torrent && !row.parts.is_empty() && row.total.is_some() {
            ui.add_space(8.0);
            ui.label(RichText::new("Parts").color(palette.text_dim));
            parts_bar(ui, palette, row);
        }
        if row.torrent && !row.files.is_empty() {
            ui.add_space(8.0);
            files(ui, palette, row, actions);
        }
        let finished_file = !row.torrent
            && row.status == Status::Finished
            && row.path.as_ref().is_some_and(|path| path.is_file());
        if finished_file {
            ui.add_space(10.0);
            checksum_row(ui, palette, row, checksum);
        }
    });
}

/// Each part of the file as a block: dark where written, light where still to fetch.
fn parts_bar(ui: &mut Ui, palette: &Palette, row: &Row) {
    let Some(total) = row.total.filter(|total| *total > 0) else {
        return;
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), palette.border);
    let x = |offset: u64| rect.left() + rect.width() * (offset.min(total) as f32 / total as f32);
    for part in row.parts.iter().filter(|part| part.end != UNKNOWN_END) {
        let done = Rect::from_x_y_ranges(x(part.start)..=x(part.pos), rect.y_range());
        painter.rect_filled(done, CornerRadius::ZERO, palette.accent);
        // A thin line where each part starts.
        if part.start > 0 {
            painter.vline(
                x(part.start),
                rect.y_range(),
                egui::Stroke::new(1.0, palette.bg),
            );
        }
    }
}

fn files(ui: &mut Ui, palette: &Palette, row: &Row, actions: &mut Vec<Action>) {
    ui.label(RichText::new(format!("Files ({})", row.files.len())).color(palette.text_dim));
    for (index, file) in row.files.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut wanted = file.wanted;
            if ui
                .checkbox(&mut wanted, "")
                .on_hover_text("Download this file")
                .changed()
            {
                actions.push(Action::Wanted(row.id, index, wanted));
            }
            let fraction = if file.size > 0 {
                file.done as f32 / file.size as f32
            } else {
                1.0
            };
            let info = format!("{}  ·  {:.0}%", format::size(file.size), fraction * 100.0);
            let width = (ui.available_width() - 150.0).max(80.0);
            ui.label(crate::list::one_line(
                ui,
                &file.path,
                13.0,
                palette.text,
                width,
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(info).color(palette.text_dim).small());
            });
        });
    }
}

fn checksum_row(ui: &mut Ui, palette: &Palette, row: &Row, checksum: &mut Option<Checksum>) {
    let mine = checksum.as_mut().filter(|checksum| checksum.id == row.id);
    let Some(checksum_state) = mine else {
        let label = with_icon(icon::FINGERPRINT, "Check SHA-256");
        if ui
            .button(label)
            .on_hover_text("Compare with the hash the site gives")
            .clicked()
            && let Some(path) = row.path.clone()
        {
            *checksum = Some(Checksum::start(ui.ctx(), row.id, path));
        }
        return;
    };
    match checksum_state.result() {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Computing SHA-256…");
            });
        }
        Some(Err(err)) => {
            ui.label(RichText::new(err).color(palette.error));
        }
        Some(Ok(hash)) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SHA-256").color(palette.text_dim));
                ui.label(RichText::new(&hash).monospace());
                if widgets::small_icon_button(ui, icon::COPY, "Copy").clicked() {
                    ui.ctx().copy_text(hash.clone());
                }
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut checksum_state.expected)
                        .hint_text("Paste the expected hash to compare")
                        .margin(widgets::FIELD_MARGIN)
                        .desired_width(420.0),
                );
                let expected = checksum_state.expected.trim();
                if !expected.is_empty() {
                    if expected.eq_ignore_ascii_case(&hash) {
                        ui.label(
                            RichText::new(with_icon(icon::CHECK_CIRCLE, "Same file")).strong(),
                        );
                    } else {
                        let text = with_icon(
                            icon::WARNING_CIRCLE,
                            "Different: the file is not the one expected",
                        );
                        ui.label(RichText::new(text).color(palette.error).strong());
                    }
                }
            });
        }
    }
}
