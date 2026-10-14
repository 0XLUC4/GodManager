//! Application state, window layout and the actions of the list.

use std::path::PathBuf;

use egui::{
    Align, Button, CentralPanel, Context, CornerRadius, Frame, Layout, Margin, Panel, RichText,
    ScrollArea, Sense, TextEdit, TextureHandle, Ui, ViewportCommand, vec2,
};
use godmanager_core::format;
use godmanager_core::kind::Kind;
use godmanager_core::manager::{Event, Manager, Row, Status};

use crate::details::{self, Checksum};
use crate::dialogs::{
    AddDialog, AddOutcome, RemoveDialog, RemoveOutcome, SettingsDialog, SettingsOutcome,
};
use crate::instance::Inbox;
use crate::theme::{self, Palette};
use crate::toasts::Toasts;
use crate::widgets::{self, FIELD_MARGIN, icon, with_icon};
use crate::{branding, list, platform, updates};

const HEADER_HEIGHT: f32 = 52.0;

/// What a click asked for, done once the frame is drawn.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    Select(u64),
    Pause(u64),
    Resume(u64),
    Restart(u64),
    Open(u64),
    Reveal(u64),
    CopyLink(u64),
    AskRemove(u64),
    Wanted(u64, usize, bool),
    CloseDetails,
}

/// Views of the sidebar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Stopped,
    Finished,
    Failed,
    Torrents,
    Kind(Kind),
}

impl Filter {
    fn matches(self, row: &Row) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => row.status.active() || row.status == Status::Seeding,
            Filter::Stopped => {
                matches!(
                    row.status,
                    Status::Paused | Status::Queued | Status::Starting
                )
            }
            Filter::Finished => matches!(row.status, Status::Finished | Status::Seeding),
            Filter::Failed => matches!(row.status, Status::Failed(_)),
            Filter::Torrents => row.torrent,
            Filter::Kind(kind) => row.kind == kind,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Filter::All => "All downloads",
            Filter::Active => "Downloading",
            Filter::Stopped => "Paused and waiting",
            Filter::Finished => "Finished",
            Filter::Failed => "Failed",
            Filter::Torrents => "Torrents",
            Filter::Kind(kind) => kind.title(),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Filter::All => icon::LIST,
            Filter::Active => icon::DOWNLOAD_SIMPLE,
            Filter::Stopped => icon::PAUSE,
            Filter::Finished => icon::CHECK_CIRCLE,
            Filter::Failed => icon::WARNING_CIRCLE,
            Filter::Torrents => icon::MAGNET,
            Filter::Kind(Kind::Video) => icon::FILM_STRIP,
            Filter::Kind(Kind::Music) => icon::MUSIC_NOTES,
            Filter::Kind(Kind::Picture) => icon::IMAGE,
            Filter::Kind(Kind::Document) => icon::FILE_TEXT,
            Filter::Kind(Kind::Archive) => icon::FILE_ZIP,
            Filter::Kind(Kind::Program) => icon::APP_WINDOW,
            Filter::Kind(Kind::Other) => icon::FILE,
        }
    }
}

pub struct GodApp {
    manager: Manager,
    dark: bool,
    logo: Option<TextureHandle>,
    toasts: Toasts,
    updates: updates::Updates,
    inbox: Inbox,
    filter: Filter,
    search: String,
    selected: Option<u64>,
    details: bool,
    add: Option<AddDialog>,
    settings: Option<SettingsDialog>,
    remove: Option<RemoveDialog>,
    checksum: Option<Checksum>,
    window_title: String,
}

impl GodApp {
    /// `args` are links or .torrent files from the command line ("Open with", a magnet click).
    pub fn new(cc: &eframe::CreationContext<'_>, args: Vec<String>, inbox: Inbox) -> Self {
        let manager = Manager::load(platform::data_dir(), &user_agent());
        let dark = manager.settings.dark;
        theme::install(&cc.egui_ctx, dark);
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        updates::add_bold_font(&mut fonts);
        cc.egui_ctx.set_fonts(fonts);
        inbox.attach(&cc.egui_ctx);
        let mut app = Self {
            manager,
            dark,
            logo: branding::logo_texture(&cc.egui_ctx),
            toasts: Toasts::default(),
            updates: updates::Updates::default(),
            inbox,
            filter: Filter::All,
            search: String::new(),
            selected: None,
            details: true,
            add: None,
            settings: None,
            remove: None,
            checksum: None,
            window_title: String::new(),
        };
        app.updates.start(&cc.egui_ctx);
        app.take_args(&args);
        app
    }

    /// Links open the Add window; .torrent files are added right away.
    fn take_args(&mut self, args: &[String]) {
        let mut links = Vec::new();
        for arg in args {
            let path = PathBuf::from(arg);
            let is_torrent = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"));
            if is_torrent && path.is_file() {
                self.add_torrent_file(&path, None, true);
            } else if !arg.trim().is_empty() {
                links.push(arg.trim().to_owned());
            }
        }
        if !links.is_empty() {
            self.open_add(links.join("\n"));
        }
    }

    fn open_add(&mut self, links: String) {
        match &mut self.add {
            Some(add) => {
                if !add.links.trim().is_empty() && !links.is_empty() {
                    add.links.push('\n');
                }
                add.links.push_str(&links);
            }
            None => self.add = Some(AddDialog::new(links, &self.manager.settings)),
        }
    }

    fn add_torrent_file(&mut self, path: &std::path::Path, folder: Option<PathBuf>, start: bool) {
        match self.manager.add_torrent_file(path, folder, start) {
            Ok(id) => self.selected = Some(id),
            Err(err) => self.toasts.error(err),
        }
    }

    fn row(&self, id: u64) -> Option<&Row> {
        self.manager.rows().iter().find(|row| row.id == id)
    }

    fn visible(&self) -> Vec<Row> {
        let needle = self.search.trim().to_lowercase();
        self.manager
            .rows()
            .iter()
            .filter(|row| self.filter.matches(row))
            .filter(|row| needle.is_empty() || row.name.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    // ------------------------------------------------------------ input

    fn handle_inbox(&mut self, ctx: &Context) {
        for args in self.inbox.take() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            self.take_args(&args);
        }
    }

    fn handle_drops(&mut self, ctx: &Context) {
        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        for file in dropped {
            let Some(path) = file.path else {
                continue;
            };
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
            {
                self.add_torrent_file(&path, None, true);
            } else {
                self.toasts.error(format!(
                    "{} is not a .torrent file",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }

    fn handle_keys(&mut self, ctx: &Context) {
        let dialog_open = self.add.is_some() || self.settings.is_some() || self.remove.is_some();
        if dialog_open {
            return;
        }
        let typing = ctx.memory(|memory| memory.focused().is_some());
        let pasted: Option<String> = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            })
        });
        if let Some(text) = pasted.filter(|_| !typing) {
            self.open_add(text.trim().to_owned());
        }
        let (add, open, delete, space) = ctx.input_mut(|input| {
            let command = egui::Modifiers::COMMAND;
            (
                input.consume_key(command, egui::Key::N),
                input.consume_key(command, egui::Key::O),
                !typing && input.key_pressed(egui::Key::Delete),
                !typing && input.key_pressed(egui::Key::Space),
            )
        });
        if add {
            self.open_add(String::new());
        }
        if open {
            self.pick_torrents();
        }
        if let Some(id) = self.selected {
            let mut actions = Vec::new();
            if delete {
                actions.push(Action::AskRemove(id));
            }
            if space && let Some(row) = self.row(id) {
                let stopped = matches!(row.status, Status::Paused | Status::Failed(_));
                actions.push(if stopped {
                    Action::Resume(id)
                } else {
                    Action::Pause(id)
                });
            }
            self.run(ctx, actions);
        }
    }

    fn pick_torrents(&mut self) {
        for path in platform::pick_torrents() {
            self.add_torrent_file(&path, None, true);
        }
    }

    // ------------------------------------------------------------ actions

    fn run(&mut self, ctx: &Context, actions: Vec<Action>) {
        for action in actions {
            let result = match action {
                Action::Select(id) => {
                    self.selected = Some(id);
                    Ok(())
                }
                Action::Pause(id) => self.manager.pause(id),
                Action::Resume(id) => self.manager.resume(id),
                Action::Restart(id) => {
                    self.manager.restart(id);
                    Ok(())
                }
                Action::Open(id) => match self.row(id).and_then(|row| row.path.clone()) {
                    Some(path) => platform::open(&path),
                    None => Err("The file is not there yet".into()),
                },
                Action::Reveal(id) => match self.row(id) {
                    Some(row) => {
                        let path = row.path.clone().unwrap_or_else(|| row.folder.clone());
                        // While downloading, the file still has its temporary name.
                        let shown = if path.exists() {
                            path
                        } else {
                            godmanager_core::http::part_path(&path)
                        };
                        platform::reveal(&shown, &row.folder)
                    }
                    None => Ok(()),
                },
                Action::CopyLink(id) => {
                    if let Some(row) = self.row(id) {
                        ctx.copy_text(row.link.clone());
                        self.toasts.success("Copied");
                    }
                    Ok(())
                }
                Action::AskRemove(id) => {
                    if let Some(row) = self.row(id) {
                        self.remove = Some(RemoveDialog {
                            id,
                            name: row.name.clone(),
                            delete_files: false,
                        });
                    }
                    Ok(())
                }
                Action::Wanted(id, file, wanted) => self.manager.set_file_wanted(id, file, wanted),
                Action::CloseDetails => {
                    self.details = false;
                    Ok(())
                }
            };
            if let Err(err) = result {
                self.toasts.error(err);
            }
        }
    }

    fn all(&mut self, pause: bool) {
        let ids: Vec<(u64, bool)> = self
            .manager
            .rows()
            .iter()
            .map(|row| (row.id, matches!(row.status, Status::Paused)))
            .collect();
        for (id, paused) in ids {
            let result = if pause && !paused {
                self.manager.pause(id)
            } else if !pause && paused {
                self.manager.resume(id)
            } else {
                Ok(())
            };
            if let Err(err) = result {
                self.toasts.error(err);
            }
        }
    }

    fn clear_finished(&mut self) {
        let finished: Vec<u64> = self
            .manager
            .rows()
            .iter()
            .filter(|row| row.status == Status::Finished)
            .map(|row| row.id)
            .collect();
        for id in finished {
            if let Err(err) = self.manager.remove(id, false) {
                self.toasts.error(err);
            }
        }
    }

    fn submit_add(&mut self, add: AddDialog) {
        let mut added = 0;
        let mut errors = Vec::new();
        let connections = self.manager.settings.connections;
        self.manager.settings.connections = add.connections;
        for line in add
            .links
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            match self
                .manager
                .add_link(line, Some(add.folder.clone()), add.start)
            {
                Ok(id) => {
                    added += 1;
                    self.selected = Some(id);
                }
                Err(err) => errors.push(err),
            }
        }
        self.manager.settings.connections = connections;
        if added > 0 {
            self.filter = Filter::All;
        }
        for err in errors {
            self.toasts.error(err);
        }
    }

    // ------------------------------------------------------------ layout

    fn header(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        let row = vec2(ui.available_width(), HEADER_HEIGHT);
        ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
            if let Some(logo) = &self.logo {
                ui.add(egui::Image::new(logo).fit_to_exact_size(vec2(26.0, 26.0)));
            }
            ui.label(RichText::new(crate::APP_NAME).size(16.0).strong());
            ui.add_space(14.0);
            let add = Button::new(
                RichText::new(with_icon(icon::PLUS, "Add"))
                    .size(14.0)
                    .color(palette.bg)
                    .strong(),
            )
            .fill(palette.accent)
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(84.0, 32.0));
            if ui.add(add).on_hover_text("Add links (Ctrl+N)").clicked() {
                self.open_add(String::new());
            }
            if widgets::icon_button(ui, icon::MAGNET, "Add a torrent file (Ctrl+O)").clicked() {
                self.pick_torrents();
            }
            ui.add_space(6.0);
            if widgets::icon_button(ui, icon::PLAY, "Resume all").clicked() {
                self.all(false);
            }
            if widgets::icon_button(ui, icon::PAUSE, "Pause all").clicked() {
                self.all(true);
            }
            if widgets::icon_button(ui, icon::BROOM, "Clear finished from the list").clicked() {
                self.clear_finished();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (glyph, tip) = if self.dark {
                    (icon::SUN, "Light mode")
                } else {
                    (icon::MOON, "Dark mode")
                };
                if widgets::icon_button(ui, glyph, tip).clicked() {
                    self.dark = !self.dark;
                    theme::set_dark(ui.ctx(), self.dark);
                    let mut settings = self.manager.settings.clone();
                    settings.dark = self.dark;
                    self.manager.set_settings(settings);
                }
                self.updates.button(ui);
                if widgets::icon_button(ui, icon::GEAR_SIX, "Settings").clicked() {
                    self.settings = Some(SettingsDialog::new(&self.manager.settings));
                }
                ui.add_space(6.0);
                let width = (ui.available_width() - 20.0).clamp(140.0, 280.0);
                ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text(format!("{}  Search", icon::MAGNIFYING_GLASS))
                        .margin(FIELD_MARGIN)
                        .desired_width(width),
                );
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        let rows = self.manager.rows();
        let count = |filter: Filter| rows.iter().filter(|row| filter.matches(row)).count();
        let mut chosen = None;
        let mut item = |ui: &mut Ui, filter: Filter, current: Filter| {
            let active = filter == current;
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
            let fill = if active {
                palette.selected
            } else if response.hovered() {
                palette.hover
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
            let painter = ui.painter();
            painter.text(
                rect.left_center() + vec2(12.0, 0.0),
                egui::Align2::LEFT_CENTER,
                filter.glyph(),
                egui::FontId::proportional(16.0),
                palette.text,
            );
            painter.text(
                rect.left_center() + vec2(38.0, 0.0),
                egui::Align2::LEFT_CENTER,
                filter.title(),
                egui::FontId::proportional(14.0),
                palette.text,
            );
            let number = count(filter);
            if number > 0 {
                painter.text(
                    rect.right_center() - vec2(12.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    number.to_string(),
                    egui::FontId::proportional(12.5),
                    palette.text_dim,
                );
            }
            if response.clicked() {
                chosen = Some(filter);
            }
        };
        ui.add_space(6.0);
        for filter in [
            Filter::All,
            Filter::Active,
            Filter::Stopped,
            Filter::Finished,
            Filter::Failed,
        ] {
            item(ui, filter, self.filter);
        }
        ui.add_space(14.0);
        ui.label(RichText::new("TYPES").size(11.5).color(palette.text_dim));
        ui.add_space(2.0);
        item(ui, Filter::Torrents, self.filter);
        for kind in Kind::ALL {
            let filter = Filter::Kind(kind);
            if count(filter) > 0 || self.filter == filter {
                item(ui, filter, self.filter);
            }
        }
        if let Some(filter) = chosen {
            self.filter = filter;
        }
    }

    fn status_bar(&self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        let (down, up) = self.manager.speeds();
        let rows = self.manager.rows();
        let active = rows.iter().filter(|row| row.status.active()).count();
        ui.horizontal(|ui| {
            let dim = |text: String| RichText::new(text).color(palette.text_dim).size(12.5);
            ui.label(dim(format!(
                "{}  {}",
                icon::DOWNLOAD_SIMPLE,
                format::speed(down)
            )));
            ui.add_space(8.0);
            ui.label(dim(format!(
                "{}  {}",
                icon::UPLOAD_SIMPLE,
                format::speed(up)
            )));
            ui.add_space(12.0);
            let plural = if rows.len() == 1 { "" } else { "s" };
            ui.label(dim(format!(
                "{active} active  ·  {} download{plural}",
                rows.len()
            )));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let settings = &self.manager.settings;
                if settings.down_limit_kb > 0 || settings.up_limit_kb > 0 {
                    ui.label(dim(format!("{}  Speed limited", icon::HOURGLASS)));
                    ui.add_space(8.0);
                }
                let engine = match (&self.manager.torrent_error, self.manager.torrents_ready()) {
                    (Some(err), _) => {
                        ui.label(
                            RichText::new(format!("{}  Torrents off", icon::WARNING_CIRCLE))
                                .color(palette.error)
                                .size(12.5),
                        )
                        .on_hover_text(err);
                        return;
                    }
                    (None, true) => "Torrents ready",
                    (None, false) => "Starting the torrent engine…",
                };
                ui.label(dim(format!("{}  {engine}", icon::MAGNET)));
            });
        });
    }

    fn list(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        let rows = self.visible();
        if rows.is_empty() {
            let filtered = !self.manager.rows().is_empty();
            if list::empty(ui, &palette, self.logo.as_ref(), filtered) {
                self.open_add(String::new());
            }
            return;
        }
        ui.add_space(4.0);
        ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            list::ROW_HEIGHT,
            rows.len(),
            |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for row in &rows[range] {
                    list::row(ui, &palette, row, self.selected == Some(row.id), actions);
                }
            },
        );
    }

    fn dialogs(&mut self, ctx: &Context) {
        if let Some(add) = &mut self.add {
            match add.show(ctx) {
                AddOutcome::Open => {}
                AddOutcome::Cancel => self.add = None,
                AddOutcome::Submit => {
                    if let Some(add) = self.add.take() {
                        self.submit_add(add);
                    }
                }
                AddOutcome::PickTorrents => {
                    let (folder, start) = (add.folder.clone(), add.start);
                    for path in platform::pick_torrents() {
                        self.add_torrent_file(&path, Some(folder.clone()), start);
                    }
                    let empty = self
                        .add
                        .as_ref()
                        .is_some_and(|add| add.links.trim().is_empty());
                    if empty {
                        self.add = None;
                    }
                }
            }
        }
        if let Some(settings) = &mut self.settings {
            match settings.show(ctx) {
                SettingsOutcome::Open => {}
                SettingsOutcome::Cancel => self.settings = None,
                SettingsOutcome::Save => {
                    if let Some(dialog) = self.settings.take() {
                        let mut draft = dialog.draft;
                        draft.dark = self.dark;
                        self.manager.set_settings(draft);
                        self.toasts.success("Settings saved");
                    }
                }
            }
        }
        if let Some(remove) = &mut self.remove {
            match remove.show(ctx) {
                RemoveOutcome::Open => {}
                RemoveOutcome::Cancel => self.remove = None,
                RemoveOutcome::Remove { delete_files } => {
                    let id = remove.id;
                    self.remove = None;
                    match self.manager.remove(id, delete_files) {
                        Ok(()) if self.selected == Some(id) => self.selected = None,
                        Ok(()) => {}
                        Err(err) => self.toasts.error(err),
                    }
                }
            }
        }
    }

    fn events(&mut self, ctx: &Context) {
        let events = self.manager.tick();
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        for event in &events {
            match event {
                Event::Finished(name) => self.toasts.success(format!("Done: {name}")),
                Event::Failed(name, err) => self.toasts.error(format!("{name}: {err}")),
            }
        }
        if !events.is_empty() && !focused {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        if let Some(err) = self.manager.save_error.take() {
            self.toasts.error(err);
        }
    }

    fn update_title(&mut self, ctx: &Context) {
        let rows = self.manager.rows();
        let running: Vec<&Row> = rows
            .iter()
            .filter(|row| row.status == Status::Downloading)
            .collect();
        let title = if running.is_empty() {
            crate::APP_NAME.to_owned()
        } else {
            let (done, total) = running.iter().fold((0u64, 0u64), |(done, total), row| {
                (done + row.done, total + row.total.unwrap_or(row.done))
            });
            let percent = if total > 0 { done * 100 / total } else { 0 };
            let (down, _) = self.manager.speeds();
            format!("{percent}% · {} — {}", format::speed(down), crate::APP_NAME)
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

fn user_agent() -> String {
    format!("{}/{}", crate::APP_NAME, env!("CARGO_PKG_VERSION"))
}

impl eframe::App for GodApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_inbox(&ctx);
        self.handle_drops(&ctx);
        self.handle_keys(&ctx);
        self.events(&ctx);
        self.update_title(&ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            self.updates.relaunch();
        }
        let palette = Palette::of(ui);
        let mut actions = Vec::new();

        Panel::top("header")
            .frame(
                Frame::new()
                    .fill(palette.surface)
                    .inner_margin(Margin::symmetric(14, 0)),
            )
            .show(ui, |ui| self.header(ui));
        Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(palette.bg)
                    .inner_margin(Margin::symmetric(14, 4)),
            )
            .show(ui, |ui| self.status_bar(ui));
        let selected = self
            .selected
            .and_then(|id| self.row(id).cloned())
            .filter(|_| self.details);
        if let Some(row) = selected {
            Panel::bottom("details")
                .resizable(true)
                .default_size(190.0)
                .size_range(120.0..=460.0)
                .frame(
                    Frame::new()
                        .fill(palette.bg)
                        .inner_margin(Margin::symmetric(16, 10)),
                )
                .show(ui, |ui| {
                    details::show(ui, &palette, &row, &mut self.checksum, &mut actions);
                });
        }
        Panel::left("sidebar")
            .resizable(false)
            .exact_size(212.0)
            .frame(
                Frame::new()
                    .fill(palette.bg)
                    .inner_margin(Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| self.sidebar(ui));
        CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(palette.bg)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| self.list(ui, &mut actions));

        let reopen_details = actions
            .iter()
            .any(|action| matches!(action, Action::Select(_)));
        self.run(&ctx, actions);
        if reopen_details {
            self.details = true;
        }
        self.dialogs(&ctx);
        self.updates.show(&ctx);
        self.toasts.show(&ctx);

        let busy = self
            .manager
            .rows()
            .iter()
            .any(|row| row.status.active() || row.status == Status::Seeding)
            || !self.manager.torrents_ready();
        let wait = if busy { 250 } else { 1000 };
        ctx.request_repaint_after(std::time::Duration::from_millis(wait));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.manager.shutdown();
    }
}
