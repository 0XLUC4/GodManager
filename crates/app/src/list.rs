//! The list of downloads: one painted line each, with its progress and its buttons.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Rect, RichText, Sense, Stroke, StrokeKind,
    TextureHandle, Ui, pos2, vec2,
};
use godmanager_core::format;
use godmanager_core::kind::Kind;
use godmanager_core::manager::{Row, Status};

use crate::app::Action;
use crate::theme::Palette;
use crate::widgets::{self, icon, with_icon};

pub const ROW_HEIGHT: f32 = 62.0;
const ICON_SIDE: f32 = 38.0;
/// Width kept on the right for the speed and the buttons.
const RIGHT_BLOCK: f32 = 250.0;

pub fn kind_glyph(row: &Row) -> &'static str {
    if row.torrent && row.files.len() > 1 {
        return icon::MAGNET;
    }
    match row.kind {
        Kind::Video => icon::FILM_STRIP,
        Kind::Music => icon::MUSIC_NOTES,
        Kind::Picture => icon::IMAGE,
        Kind::Document => icon::FILE_TEXT,
        Kind::Archive => icon::FILE_ZIP,
        Kind::Program => icon::APP_WINDOW,
        Kind::Other if row.torrent => icon::MAGNET,
        Kind::Other => icon::FILE,
    }
}

/// One line of text cut with "…" to fit `width`.
pub fn one_line(ui: &Ui, text: &str, size: f32, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(size), color);
    job.wrap = TextWrapping::truncate_at_width(width.max(10.0));
    ui.painter().layout_job(job)
}

/// "34 MB of 1.2 GB · 3 min left · 8 connections".
pub fn summary(row: &Row) -> String {
    let mut parts: Vec<String> = Vec::new();
    match &row.status {
        Status::Failed(err) => return err.clone(),
        Status::Finished | Status::Seeding => {
            if let Some(total) = row.total {
                parts.push(format::size(total));
            }
            if row.status == Status::Seeding {
                parts.push(format!("Sharing with {} peers", row.peers));
                parts.push(format!("{} sent", format::size(row.uploaded)));
            }
            return parts.join("  ·  ");
        }
        status => parts.push(status.label().to_owned()),
    }
    match row.total {
        Some(total) => parts.push(format!(
            "{} of {}",
            format::size(row.done),
            format::size(total)
        )),
        None if row.done > 0 => parts.push(format::size(row.done)),
        None => {}
    }
    if let Some(eta) = row.eta.filter(|_| row.status == Status::Downloading) {
        parts.push(format!("{} left", format::duration(eta)));
    }
    if row.status == Status::Downloading {
        if row.torrent {
            parts.push(format!("{} peers", row.peers));
        } else if row.connections > 0 {
            let plural = if row.connections == 1 { "" } else { "s" };
            parts.push(format!("{} connection{plural}", row.connections));
        }
    }
    parts.join("  ·  ")
}

pub fn row(ui: &mut Ui, palette: &Palette, row: &Row, selected: bool, actions: &mut Vec<Action>) {
    let width = ui.available_width();
    // The buttons are put at fixed places, which moves the cursor: the row claims its
    // height last.
    let rect = Rect::from_min_size(ui.cursor().min, vec2(width, ROW_HEIGHT));
    let response = ui.interact(rect, ui.id().with(("download", row.id)), Sense::click());
    let painter = ui.painter_at(rect);
    let card = rect.shrink2(vec2(4.0, 2.0));
    let fill = if selected {
        palette.selected
    } else if response.hovered() {
        palette.hover
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(card, CornerRadius::same(10), fill);

    // File kind tile.
    let tile = Rect::from_min_size(
        pos2(card.left() + 10.0, card.center().y - ICON_SIDE / 2.0),
        vec2(ICON_SIDE, ICON_SIDE),
    );
    painter.rect(
        tile,
        CornerRadius::same(9),
        palette.surface,
        Stroke::new(1.0, palette.border),
        StrokeKind::Inside,
    );
    painter.text(
        tile.center(),
        Align2::CENTER_CENTER,
        kind_glyph(row),
        FontId::proportional(19.0),
        palette.text,
    );

    let text_left = tile.right() + 12.0;
    let text_width = (card.right() - RIGHT_BLOCK - text_left).max(60.0);
    let name = one_line(ui, &row.name, 14.5, palette.text, text_width);
    painter.galley(pos2(text_left, card.top() + 10.0), name, palette.text);
    let failed = matches!(row.status, Status::Failed(_));
    let line = summary(row);
    let line = if failed {
        format!("{}  {line}", icon::WARNING_CIRCLE)
    } else {
        line
    };
    let detail_color = if failed {
        palette.error
    } else {
        palette.text_dim
    };
    let detail = one_line(ui, &line, 12.0, detail_color, text_width);
    painter.galley(pos2(text_left, card.top() + 30.0), detail, detail_color);

    let finished = matches!(row.status, Status::Finished | Status::Seeding);
    if !finished && !failed {
        let bar = Rect::from_min_size(pos2(text_left, card.bottom() - 11.0), vec2(text_width, 4.0));
        progress_bar(ui, palette, bar, row.fraction(), row.status.active());
    }

    // Speed, right-aligned before the buttons.
    let buttons_left = card.right() - 3.0 * widgets::ICON_BUTTON.x - 16.0;
    let speed = match &row.status {
        Status::Downloading => Some(format::speed(row.down_bps)),
        Status::Seeding if row.up_bps > 0.0 => Some(format!(
            "{} {}",
            icon::UPLOAD_SIMPLE,
            format::speed(row.up_bps)
        )),
        _ => None,
    };
    if let Some(speed) = speed {
        painter.text(
            pos2(buttons_left - 12.0, card.center().y),
            Align2::RIGHT_CENTER,
            speed,
            FontId::proportional(13.0),
            palette.text,
        );
    } else if let Some(fraction) = row.fraction().filter(|_| !finished && !failed) {
        painter.text(
            pos2(buttons_left - 12.0, card.center().y),
            Align2::RIGHT_CENTER,
            format!("{:.0}%", fraction * 100.0),
            FontId::proportional(13.0),
            palette.text_dim,
        );
    }

    let button_rect = |index: usize| {
        let size = widgets::ICON_BUTTON;
        let left = buttons_left + index as f32 * (size.x + 4.0);
        Rect::from_min_size(pos2(left, card.center().y - size.y / 2.0), size)
    };
    let (glyph, tip, action) = primary(row);
    let primary_button = ui
        .put(button_rect(0), widgets::icon_widget(glyph, 16.0))
        .on_hover_text(tip);
    if primary_button.clicked() {
        actions.push(action);
    }
    let folder = ui
        .put(
            button_rect(1),
            widgets::icon_widget(icon::FOLDER_OPEN, 16.0),
        )
        .on_hover_text("Show in folder");
    if folder.clicked() {
        actions.push(Action::Reveal(row.id));
    }
    let more = ui
        .put(button_rect(2), widgets::icon_widget(icon::DOTS_THREE, 16.0))
        .on_hover_text("More");
    egui::Popup::menu(&more).show(|ui| menu(ui, row, actions));

    if response.clicked() || response.secondary_clicked() {
        actions.push(Action::Select(row.id));
    }
    if response.double_clicked() {
        actions.push(if finished {
            Action::Open(row.id)
        } else {
            Action::Reveal(row.id)
        });
    }
    response.context_menu(|ui| menu(ui, row, actions));
    ui.advance_cursor_after_rect(rect);
}

/// Button that does what the download most needs: pause, go on, or try again.
fn primary(row: &Row) -> (&'static str, &'static str, Action) {
    match row.status {
        Status::Failed(_) => (icon::ARROW_CLOCKWISE, "Try again", Action::Resume(row.id)),
        Status::Finished if row.torrent => (icon::PLAY, "Share again", Action::Resume(row.id)),
        Status::Finished => (icon::FILE, "Open", Action::Open(row.id)),
        Status::Paused => (icon::PLAY, "Resume", Action::Resume(row.id)),
        Status::Seeding => (icon::PAUSE, "Stop sharing", Action::Pause(row.id)),
        _ => (icon::PAUSE, "Pause", Action::Pause(row.id)),
    }
}

pub fn menu(ui: &mut Ui, row: &Row, actions: &mut Vec<Action>) {
    ui.set_min_width(210.0);
    let finished = matches!(row.status, Status::Finished | Status::Seeding);
    let mut item = |ui: &mut Ui, enabled: bool, glyph: &str, label: &str, action: Action| {
        if ui
            .add_enabled(enabled, egui::Button::new(with_icon(glyph, label)))
            .clicked()
        {
            actions.push(action);
            ui.close();
        }
    };
    item(ui, finished, icon::FILE, "Open", Action::Open(row.id));
    item(
        ui,
        true,
        icon::FOLDER_OPEN,
        "Show in folder",
        Action::Reveal(row.id),
    );
    ui.separator();
    let paused = matches!(row.status, Status::Paused | Status::Failed(_));
    if paused || (row.torrent && row.status == Status::Finished) {
        item(ui, true, icon::PLAY, "Resume", Action::Resume(row.id));
    } else {
        item(
            ui,
            !finished || row.torrent,
            icon::PAUSE,
            "Pause",
            Action::Pause(row.id),
        );
    }
    if !row.torrent {
        item(
            ui,
            true,
            icon::ARROW_CLOCKWISE,
            "Download again",
            Action::Restart(row.id),
        );
    }
    let copy_label = if row.torrent {
        "Copy magnet link"
    } else {
        "Copy address"
    };
    item(ui, true, icon::LINK, copy_label, Action::CopyLink(row.id));
    ui.separator();
    item(
        ui,
        true,
        icon::X,
        "Remove from list",
        Action::AskRemove(row.id),
    );
}

pub fn progress_bar(ui: &Ui, palette: &Palette, rect: Rect, fraction: Option<f32>, active: bool) {
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(2),
        palette.text.gamma_multiply(0.14),
    );
    let fill = if active {
        palette.accent
    } else {
        palette.text_dim
    };
    match fraction {
        Some(fraction) => {
            let done = Rect::from_min_size(rect.min, vec2(rect.width() * fraction, rect.height()));
            painter.rect_filled(done, CornerRadius::same(2), fill);
        }
        // Size unknown: a short piece slides along the bar.
        None if active => {
            let time = ui.input(|input| input.time) as f32;
            let piece = rect.width() * 0.25;
            let left = rect.left() + (time * 0.6).fract() * (rect.width() - piece);
            let moving = Rect::from_min_size(pos2(left, rect.top()), vec2(piece, rect.height()));
            painter.rect_filled(moving, CornerRadius::same(2), fill);
        }
        None => {}
    }
}

/// Shown while the list is empty.
pub fn empty(ui: &mut Ui, palette: &Palette, logo: Option<&TextureHandle>, filtered: bool) -> bool {
    let mut add = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 170.0).max(24.0));
        if filtered {
            ui.label(
                RichText::new(icon::MAGNIFYING_GLASS)
                    .size(40.0)
                    .color(palette.text_dim),
            );
            ui.add_space(8.0);
            ui.label(RichText::new("Nothing here").size(18.0).strong());
            ui.label(RichText::new("No download matches this view.").color(palette.text_dim));
            return;
        }
        if let Some(logo) = logo {
            ui.add(
                egui::Image::new(logo)
                    .fit_to_exact_size(vec2(112.0, 112.0))
                    .corner_radius(CornerRadius::same(24)),
            );
        }
        ui.add_space(10.0);
        ui.label(RichText::new(crate::APP_NAME).size(40.0).strong());
        let tagline = "Every file, every torrent. Local. Pure Rust.";
        ui.label(RichText::new(tagline).size(17.0).color(palette.text_dim));
        ui.add_space(24.0);
        let button = egui::Button::new(
            RichText::new(with_icon(icon::PLUS, "Add a download"))
                .size(16.0)
                .color(palette.bg),
        )
        .fill(palette.accent)
        .corner_radius(CornerRadius::same(10))
        .min_size(vec2(190.0, 44.0));
        add = ui.add(button).clicked();
        ui.add_space(14.0);
        let hint = "Paste a link with Ctrl+V, or drop a .torrent file on the window";
        ui.label(RichText::new(hint).color(palette.text_dim));
    });
    add
}
