//! The window: three panes, their tabs, and the sessions and settings dialogs.

use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, ColorImage, Context, CornerRadius, CursorIcon, Event, FontData,
    FontDefinitions, FontFamily, Frame, Id, Key, Layout, Rect, RichText, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, UiBuilder, ViewportCommand, Visuals, pos2, vec2,
};

use crate::btop::{BtopPane, BtopState};
use crate::config::{Config, SshSession};
use crate::input;
use crate::render::{self, FONT_BOLD, FONT_REGULAR, PaintOptions};
use crate::ssh::SshTarget;
use crate::terminal::{Launch, Terminal};
use crate::theme::{self, Theme};

const HEADER_HEIGHT: f32 = 30.0;
const GAP: f32 = 6.0;
const TERMINAL_PADDING: f32 = 4.0;
/// btop's default layout needs at least this much room.
const BTOP_COLS: f32 = 80.0;
const BTOP_ROWS: f32 = 24.0;
const KEYBOARD_ID: &str = "terminal-keyboard-focus";
const MACOS: bool = cfg!(target_os = "macos");

const REGULAR_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/System/Library/Fonts/Menlo.ttc",
];
const BOLD_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Bold.ttf",
    "/System/Library/Fonts/Menlo.ttc",
];

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Main,
    Btop,
    Local,
}

struct MainTab {
    target: Option<SshTarget>,
    terminal: Terminal,
    btop: BtopPane,
    ended: bool,
}

impl MainTab {
    fn title(&self) -> String {
        match &self.target {
            Some(target) => target.session.display_name(),
            None => self
                .terminal
                .title
                .clone()
                .unwrap_or_else(|| "local".into()),
        }
    }
}

impl Drop for MainTab {
    fn drop(&mut self) {
        if let Some(target) = self.target.take() {
            std::thread::spawn(move || target.close_master());
        }
    }
}

enum Action {
    NewMainLocal,
    Connect(SshSession),
    CloseMain(usize),
    Reconnect(usize),
    NewLocal,
    CloseLocal(usize),
    RefreshBtop,
}

/// The sessions dialog's edit form: which entry it edits, if any, and the draft.
struct SessionEditor {
    index: Option<usize>,
    draft: SshSession,
    error: Option<String>,
}

pub struct App {
    config: Config,
    theme: Theme,
    main_tabs: Vec<MainTab>,
    active_main: usize,
    local_tabs: Vec<Terminal>,
    active_local: usize,
    focus: Pane,
    actions: Vec<Action>,
    sessions_open: bool,
    settings_open: bool,
    editor: Option<SessionEditor>,
    background: Option<TextureHandle>,
    settings_message: Option<String>,
    status: Option<String>,
    /// Set when we asked for a real paste; a bare Ctrl+V is passed through.
    paste_requested: Option<Instant>,
    dirty_since: Option<Instant>,
    fullscreen: bool,
}

fn install_fonts(ctx: &Context, custom_path: &str) -> Option<String> {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let mut warning = None;

    let mut load = |name: &str, candidates: &[&str]| -> Option<String> {
        let (path, bytes) = candidates
            .iter()
            .find_map(|path| Some((*path, std::fs::read(path).ok()?)))?;
        let mut data = FontData::from_owned(bytes);
        // Menlo ships as a collection, with bold as its second face.
        data.index = u32::from(name == "system-bold" && path.ends_with(".ttc"));
        fonts.font_data.insert(name.to_string(), data.into());
        Some(name.to_string())
    };

    let custom_path = custom_path.trim();
    let mut custom = None;
    if !custom_path.is_empty() {
        custom = load("custom", &[custom_path]);
        if custom.is_none() {
            warning = Some(format!("Could not read the font at {custom_path}"));
        }
    }
    let regular = load("system-regular", REGULAR_FONTS);
    let bold = load("system-bold", BOLD_FONTS);

    let chain = |first: Vec<Option<String>>| -> Vec<String> {
        first
            .into_iter()
            .flatten()
            .chain(fallbacks.iter().cloned())
            .collect()
    };
    let regular_chain = chain(vec![custom.clone(), regular.clone()]);
    // With a custom font the system bold wouldn't match, so use the custom one.
    let bold_chain = if custom.is_some() {
        chain(vec![custom, regular])
    } else {
        chain(vec![bold, regular])
    };
    fonts
        .families
        .insert(FontFamily::Name(FONT_REGULAR.into()), regular_chain);
    fonts
        .families
        .insert(FontFamily::Name(FONT_BOLD.into()), bold_chain);
    ctx.set_fonts(fonts);
    warning
}

fn load_background(ctx: &Context, path: &str) -> Result<Option<TextureHandle>, String> {
    const MAX_DIMENSION: u32 = 4096;
    let path = path.trim();
    if path.is_empty() {
        return Ok(None);
    }
    let mut image = image::open(path).map_err(|e| format!("Could not load image: {e}"))?;
    if image.width() > MAX_DIMENSION || image.height() > MAX_DIMENSION {
        image = image.thumbnail(MAX_DIMENSION, MAX_DIMENSION);
    }
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Ok(Some(ctx.load_texture(
        "background",
        color_image,
        TextureOptions::LINEAR,
    )))
}

fn apply_visuals(ctx: &Context, theme: &Theme) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = theme.background;
    visuals.window_fill = theme.background;
    visuals.selection.bg_fill = theme.selection;
    visuals.override_text_color = Some(theme.foreground);
    ctx.set_visuals(visuals);
}

fn truncate(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let mut short: String = title.chars().take(max - 1).collect();
    short.push('…');
    short
}

enum TabEvent {
    Select(usize),
    Close(usize),
}

fn tab_strip(ui: &mut Ui, titles: &[String], active: usize) -> Option<TabEvent> {
    let mut event = None;
    for (index, title) in titles.iter().enumerate() {
        let label = truncate(title, 22);
        if ui
            .selectable_label(index == active, label)
            .on_hover_text(title)
            .clicked()
        {
            event = Some(TabEvent::Select(index));
        }
        if ui.small_button("×").on_hover_text("Close tab").clicked() {
            event = Some(TabEvent::Close(index));
        }
        ui.add_space(4.0);
    }
    event
}

/// Draws the pane's chrome. Returns the header and body areas.
fn pane_frame(ui: &Ui, rect: Rect, focused: bool, theme: &Theme, strength: f32) -> (Rect, Rect) {
    let radius = CornerRadius::same(8);
    let fill = theme.background.gamma_multiply(1.0 - strength);
    let border = if focused {
        theme.cursor
    } else {
        theme.foreground.gamma_multiply(0.25)
    };
    ui.painter().rect(
        rect,
        radius,
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_HEIGHT));
    let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
    ui.painter()
        .hline(rect.x_range(), header.bottom(), Stroke::new(1.0, border));
    (header, body)
}

struct TerminalView<'a> {
    events: &'a [Event],
    theme: &'a Theme,
    font_size: f32,
    focused: bool,
    id: Id,
}

/// Lays out, paints and takes pointer input for a terminal. Returns true if
/// the user pressed a mouse button on it.
fn show_terminal(
    ui: &mut Ui,
    rect: Rect,
    terminal: &mut Terminal,
    view: &TerminalView<'_>,
) -> bool {
    let ctx = ui.ctx().clone();
    let cell = render::cell_size(&ctx, view.font_size);
    let inner = rect.shrink(TERMINAL_PADDING);
    let cols = (inner.width() / cell.width).floor() as usize;
    let rows = (inner.height() / cell.height).floor() as usize;
    terminal.resize(cols, rows, cell);
    terminal.set_focused(view.focused);

    let response = ui.interact(rect, view.id, Sense::click_and_drag());
    let hovered = response.contains_pointer();
    if hovered {
        ctx.set_cursor_icon(CursorIcon::Text);
    }
    let (hover_pos, now, pressed) =
        ctx.input(|i| (i.pointer.hover_pos(), i.time, i.pointer.any_pressed()));
    terminal.handle_pointer(view.events, inner, hovered, cell, hover_pos, now);

    let painter = ui.painter_at(rect);
    let options = PaintOptions {
        theme: view.theme,
        font_size: view.font_size,
        cell,
        focused: view.focused,
    };
    render::paint(&painter, inner.min, terminal, &options);
    hovered && pressed
}

impl App {
    pub fn new(ctx: &Context) -> Self {
        let (config, warning) = Config::load();
        let theme = Theme::named(
            &config.theme,
            config.foreground_override,
            config.background_override,
        );
        apply_visuals(ctx, &theme);
        ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let mut status = warning;
        if let Some(font_warning) = install_fonts(ctx, &config.font_path) {
            status = Some(font_warning);
        }
        let background = match load_background(ctx, &config.background_image) {
            Ok(texture) => texture,
            Err(err) => {
                status = Some(err);
                None
            }
        };

        let sessions_open = !config.sessions.is_empty();
        let mut app = Self {
            config,
            theme,
            main_tabs: Vec::new(),
            active_main: 0,
            local_tabs: Vec::new(),
            active_local: 0,
            focus: Pane::Main,
            actions: Vec::new(),
            sessions_open,
            settings_open: false,
            editor: None,
            background,
            settings_message: None,
            status,
            paste_requested: None,
            dirty_since: None,
            fullscreen: false,
        };
        app.open_main(ctx, None);
        app.open_local(ctx);
        app
    }

    fn spawn_main(&mut self, ctx: &Context, target: Option<&SshTarget>) -> Option<Terminal> {
        let launch = match target {
            Some(target) => Launch::command("ssh", target.main_args()),
            None => Launch::shell(&self.config.local_shell),
        };
        match Terminal::spawn(ctx, launch, self.config.scrollback) {
            Ok(terminal) => Some(terminal),
            Err(err) => {
                self.status = Some(format!("Could not start the terminal: {err}"));
                None
            }
        }
    }

    fn open_main(&mut self, ctx: &Context, session: Option<SshSession>) {
        let target = session.map(SshTarget::new);
        if let Some(terminal) = self.spawn_main(ctx, target.as_ref()) {
            let btop = BtopPane::new(target.clone());
            self.main_tabs.push(MainTab {
                target,
                terminal,
                btop,
                ended: false,
            });
            self.active_main = self.main_tabs.len() - 1;
            self.focus = Pane::Main;
        }
    }

    fn open_local(&mut self, ctx: &Context) {
        match Terminal::spawn(
            ctx,
            Launch::shell(&self.config.local_shell),
            self.config.scrollback,
        ) {
            Ok(terminal) => {
                self.local_tabs.push(terminal);
                self.active_local = self.local_tabs.len() - 1;
            }
            Err(err) => self.status = Some(format!("Could not start the terminal: {err}")),
        }
    }

    fn close_main(&mut self, index: usize) {
        if index < self.main_tabs.len() {
            self.main_tabs.remove(index);
            self.active_main = self.active_main.min(self.main_tabs.len().saturating_sub(1));
        }
    }

    fn close_local(&mut self, index: usize) {
        if index < self.local_tabs.len() {
            self.local_tabs.remove(index);
            self.active_local = self
                .active_local
                .min(self.local_tabs.len().saturating_sub(1));
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    fn save_now(&mut self) {
        self.dirty_since = None;
        if let Err(err) = self.config.save() {
            self.status = Some(format!("Could not save settings: {err}"));
        }
    }

    fn poll_terminals(&mut self, ctx: &Context) {
        let theme = &self.theme;
        let scrollback = self.config.scrollback;
        for (index, tab) in self.main_tabs.iter_mut().enumerate() {
            tab.terminal.poll(ctx, theme);
            if tab.terminal.exit.is_some() && !tab.ended {
                tab.ended = true;
                match &tab.target {
                    // A closed local shell has nothing to show, but an ssh tab
                    // stays open so errors are visible and it can reconnect.
                    None => self.actions.push(Action::CloseMain(index)),
                    Some(target) => {
                        tab.btop.session_ended();
                        let target = target.clone();
                        std::thread::spawn(move || target.close_master());
                    }
                }
            }
            tab.btop.poll(ctx, theme, scrollback);
        }
        for (index, terminal) in self.local_tabs.iter_mut().enumerate() {
            terminal.poll(ctx, theme);
            if terminal.exit.is_some() {
                self.actions.push(Action::CloseLocal(index));
            }
        }
    }

    fn run_actions(&mut self, ctx: &Context) {
        // Sorted so that removing several tabs doesn't shift later indices.
        let mut actions = std::mem::take(&mut self.actions);
        actions.sort_by_key(|action| match action {
            Action::CloseMain(index) | Action::CloseLocal(index) => std::cmp::Reverse(*index),
            _ => std::cmp::Reverse(0),
        });
        for action in actions {
            match action {
                Action::NewMainLocal => self.open_main(ctx, None),
                Action::Connect(session) => self.open_main(ctx, Some(session)),
                Action::CloseMain(index) => self.close_main(index),
                Action::Reconnect(index) => self.reconnect(ctx, index),
                Action::NewLocal => self.open_local(ctx),
                Action::CloseLocal(index) => self.close_local(index),
                Action::RefreshBtop => {
                    if let Some(tab) = self.main_tabs.get_mut(self.active_main)
                        && !tab.ended
                    {
                        tab.btop.refresh();
                    }
                }
            }
        }
    }

    fn reconnect(&mut self, ctx: &Context, index: usize) {
        let target = self.main_tabs.get(index).and_then(|tab| tab.target.clone());
        if let Some(terminal) = self.spawn_main(ctx, target.as_ref())
            && let Some(tab) = self.main_tabs.get_mut(index)
        {
            tab.terminal = terminal;
            tab.ended = false;
            tab.btop.refresh();
        }
    }

    fn focused_terminal(&mut self) -> Option<&mut Terminal> {
        match self.focus {
            Pane::Main => self
                .main_tabs
                .get_mut(self.active_main)
                .map(|t| &mut t.terminal),
            Pane::Local => self.local_tabs.get_mut(self.active_local),
            Pane::Btop => match self
                .main_tabs
                .get_mut(self.active_main)
                .map(|t| &mut t.btop.state)
            {
                Some(BtopState::Running(terminal)) => Some(terminal),
                _ => None,
            },
        }
    }

    fn cycle_tab(&mut self, backwards: bool) {
        let step = |active: usize, len: usize| {
            if len == 0 {
                0
            } else if backwards {
                (active + len - 1) % len
            } else {
                (active + 1) % len
            }
        };
        match self.focus {
            Pane::Local => self.active_local = step(self.active_local, self.local_tabs.len()),
            _ => self.active_main = step(self.active_main, self.main_tabs.len()),
        }
    }

    /// Handles an app shortcut. Returns true if the key press was consumed.
    fn shortcut(&mut self, ctx: &Context, key: Key, physical: Key, mods: egui::Modifiers) -> bool {
        if key == Key::F11 && mods.is_none() {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
            return true;
        }
        if key == Key::Tab && mods.ctrl {
            self.cycle_tab(mods.shift);
            return true;
        }
        let command = if MACOS {
            mods.mac_cmd
        } else {
            mods.ctrl && mods.shift
        };
        if !command {
            return false;
        }
        match physical {
            Key::C => {
                if let Some(text) = self.focused_terminal().and_then(|t| t.selection_text()) {
                    ctx.copy_text(text);
                }
            }
            Key::V => {
                self.paste_requested = Some(Instant::now());
                ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
                ctx.request_repaint();
            }
            Key::T => self.actions.push(match self.focus {
                Pane::Local => Action::NewLocal,
                _ => Action::NewMainLocal,
            }),
            Key::W => self.actions.push(match self.focus {
                Pane::Local => Action::CloseLocal(self.active_local),
                _ => Action::CloseMain(self.active_main),
            }),
            Key::R => self.actions.push(Action::RefreshBtop),
            Key::Num1 => self.focus = Pane::Main,
            Key::Num2 => self.focus = Pane::Btop,
            Key::Num3 => self.focus = Pane::Local,
            _ => return false,
        }
        true
    }

    fn keyboard(&mut self, ctx: &Context, events: &[Event]) {
        const CONTROL_C: u8 = 0x03;
        const CONTROL_V: u8 = 0x16;
        const CONTROL_X: u8 = 0x18;
        const PASTE_WINDOW: Duration = Duration::from_millis(500);

        let alt = ctx.input(|i| i.modifiers.alt);
        for event in events {
            let mut output: Option<Vec<u8>> = None;
            match event {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let physical = physical_key.unwrap_or(*key);
                    if self.shortcut(ctx, *key, physical, *modifiers) {
                        continue;
                    }
                    let app_cursor = self.focused_terminal().is_some_and(|t| {
                        t.mode()
                            .contains(alacritty_terminal::term::TermMode::APP_CURSOR)
                    });
                    output = input::key_bytes(*key, *modifiers, app_cursor);
                }
                Event::Text(text) => {
                    let mut bytes = Vec::new();
                    // On macOS, Option is how characters like @ are typed.
                    if alt && text.is_ascii() && !MACOS {
                        bytes.push(0x1b);
                    }
                    bytes.extend_from_slice(text.as_bytes());
                    output = Some(bytes);
                }
                // On macOS these events come from Cmd, so they really are clipboard
                // actions, and Ctrl+C and friends arrive as ordinary key presses.
                Event::Copy if MACOS => {
                    if let Some(text) = self.focused_terminal().and_then(|t| t.selection_text()) {
                        ctx.copy_text(text);
                    }
                }
                Event::Cut | Event::PasteImage(_) if MACOS => {}
                Event::Paste(text) if MACOS => {
                    if let Some(terminal) = self.focused_terminal() {
                        terminal.paste(text);
                    }
                }
                // Elsewhere egui turns these control keys into clipboard events.
                Event::Copy => output = Some(vec![CONTROL_C]),
                Event::Cut => output = Some(vec![CONTROL_X]),
                Event::PasteImage(_) => output = Some(vec![CONTROL_V]),
                Event::Paste(text) => {
                    let requested = self
                        .paste_requested
                        .take()
                        .is_some_and(|at| at.elapsed() < PASTE_WINDOW);
                    if requested {
                        if let Some(terminal) = self.focused_terminal() {
                            terminal.paste(text);
                        }
                    } else {
                        output = Some(vec![CONTROL_V]);
                    }
                }
                _ => {}
            }
            if let Some(bytes) = output
                && let Some(terminal) = self.focused_terminal()
            {
                terminal.scroll_to_bottom();
                terminal.write(bytes);
            }
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("TerminalOmega").strong());
            ui.separator();
            if ui.button("SSH Sessions").clicked() {
                self.sessions_open = !self.sessions_open;
            }
            if ui.button("Settings").clicked() {
                self.settings_open = !self.settings_open;
            }
            let label = if self.fullscreen {
                "Exit Fullscreen"
            } else {
                "Fullscreen"
            };
            if ui.button(label).on_hover_text("F11").clicked() {
                self.fullscreen = !self.fullscreen;
                ui.ctx()
                    .send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
            }
            if let Some(status) = self.status.clone() {
                ui.separator();
                ui.colored_label(self.theme.ansi[3], status);
                if ui.small_button("×").clicked() {
                    self.status = None;
                }
            }
        });
    }

    fn divider(&mut self, ui: &mut Ui, rect: Rect, id: &str, horizontal_split: bool, area: Rect) {
        let response = ui.interact(rect, Id::unique(id), Sense::drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(if horizontal_split {
                CursorIcon::ResizeHorizontal
            } else {
                CursorIcon::ResizeVertical
            });
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            if horizontal_split {
                self.config.split_x = ((pos.x - area.left()) / area.width()).clamp(0.2, 0.8);
            } else {
                self.config.split_y = ((pos.y - area.top()) / area.height()).clamp(0.2, 0.8);
            }
            self.mark_dirty();
        }
    }

    fn strength(&self) -> f32 {
        if self.background.is_some() {
            self.config.background_image_strength
        } else {
            0.0
        }
    }

    fn main_pane(&mut self, ui: &mut Ui, rect: Rect, events: &[Event]) {
        let focused = self.focus == Pane::Main;
        let (header, body) = pane_frame(ui, rect, focused, &self.theme, self.strength());

        ui.scope_builder(
            UiBuilder::new().max_rect(header.shrink2(vec2(8.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    let titles: Vec<String> = self.main_tabs.iter().map(MainTab::title).collect();
                    match tab_strip(ui, &titles, self.active_main) {
                        Some(TabEvent::Select(index)) => self.active_main = index,
                        Some(TabEvent::Close(index)) => self.actions.push(Action::CloseMain(index)),
                        None => {}
                    }
                    ui.menu_button("+", |ui| {
                        if ui.button("Local shell").clicked() {
                            self.actions.push(Action::NewMainLocal);
                            ui.close();
                        }
                        if !self.config.sessions.is_empty() {
                            ui.separator();
                        }
                        for session in &self.config.sessions {
                            if ui.button(session.display_name()).clicked() {
                                self.actions.push(Action::Connect(session.clone()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Manage sessions…").clicked() {
                            self.sessions_open = true;
                            ui.close();
                        }
                    });
                });
            },
        );

        let active = self.active_main;
        let Some(tab) = self.main_tabs.get_mut(active) else {
            centered_message(
                ui,
                body,
                "No terminal open. Use + to open a shell or connect to a server.",
            );
            return;
        };

        let mut body = body;
        if tab.ended {
            let bar = Rect::from_min_size(body.min, vec2(body.width(), HEADER_HEIGHT));
            body = Rect::from_min_max(pos2(body.left(), bar.bottom()), body.max);
            ui.scope_builder(
                UiBuilder::new().max_rect(bar.shrink2(vec2(8.0, 0.0))),
                |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.label("Session ended.");
                        if ui.button("Reconnect").clicked() {
                            self.actions.push(Action::Reconnect(active));
                        }
                        if ui.button("Close").clicked() {
                            self.actions.push(Action::CloseMain(active));
                        }
                    });
                },
            );
        }

        let view = TerminalView {
            events,
            theme: &self.theme,
            font_size: self.config.font_size,
            focused,
            id: Id::unique("main-terminal"),
        };
        if show_terminal(ui, body, &mut tab.terminal, &view) {
            self.focus = Pane::Main;
        }
    }

    fn btop_pane(&mut self, ui: &mut Ui, rect: Rect, events: &[Event]) {
        let focused = self.focus == Pane::Btop;
        let (header, body) = pane_frame(ui, rect, focused, &self.theme, self.strength());
        let active = self.main_tabs.get(self.active_main);
        let title = active.map_or("no session".to_string(), MainTab::title);
        let can_refresh = active.is_some_and(|tab| !tab.ended);

        ui.scope_builder(
            UiBuilder::new().max_rect(header.shrink2(vec2(8.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(format!("btop — {}", truncate(&title, 40)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let button = ui.add_enabled(can_refresh, egui::Button::new("Refresh"));
                        if button
                            .on_hover_text("Run btop again (Ctrl+Shift+R)")
                            .clicked()
                        {
                            self.actions.push(Action::RefreshBtop);
                        }
                    });
                });
            },
        );

        let Some(tab) = self.main_tabs.get_mut(self.active_main) else {
            centered_message(ui, body, "Open a terminal to see its resource usage here.");
            return;
        };
        let host = tab.target.as_ref().map(|t| t.session.host.clone());

        match &mut tab.btop.state {
            BtopState::Running(terminal) => {
                let area = body.shrink(TERMINAL_PADDING).size();
                let ctx = ui.ctx().clone();
                let font_size =
                    render::fit_font_size(&ctx, self.config.font_size, area, BTOP_COLS, BTOP_ROWS);
                let view = TerminalView {
                    events,
                    theme: &self.theme,
                    font_size,
                    focused,
                    id: Id::unique("btop-terminal"),
                };
                if show_terminal(ui, body, terminal, &view) {
                    self.focus = Pane::Btop;
                }
            }
            BtopState::WaitingForLogin => {
                centered_message(ui, body, "Waiting for login in the main terminal…");
            }
            BtopState::SessionEnded => {
                centered_message(ui, body, "The session has ended.");
            }
            state => {
                let machine = host.unwrap_or_else(|| "this machine".to_string());
                let message = match state {
                    BtopState::NotInstalled => format!("btop is not installed on {machine}."),
                    BtopState::Failed(message) => message.clone(),
                    _ => "btop was closed.".to_string(),
                };
                let hint = matches!(state, BtopState::NotInstalled)
                    .then_some("Install it in the main terminal, then refresh.");
                ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                    ui.with_layout(
                        Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(body.height() / 2.0 - 40.0);
                                ui.label(RichText::new(message).strong());
                                if let Some(hint) = hint {
                                    ui.label(hint);
                                }
                                ui.add_space(6.0);
                                if ui.button("Refresh").clicked() {
                                    self.actions.push(Action::RefreshBtop);
                                }
                            });
                        },
                    );
                });
            }
        }
    }

    fn local_pane(&mut self, ui: &mut Ui, rect: Rect, events: &[Event]) {
        let focused = self.focus == Pane::Local;
        let (header, body) = pane_frame(ui, rect, focused, &self.theme, self.strength());

        ui.scope_builder(
            UiBuilder::new().max_rect(header.shrink2(vec2(8.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    let titles: Vec<String> = self
                        .local_tabs
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            t.title
                                .clone()
                                .unwrap_or_else(|| format!("shell {}", i + 1))
                        })
                        .collect();
                    match tab_strip(ui, &titles, self.active_local) {
                        Some(TabEvent::Select(index)) => self.active_local = index,
                        Some(TabEvent::Close(index)) => {
                            self.actions.push(Action::CloseLocal(index))
                        }
                        None => {}
                    }
                    if ui.button("+").on_hover_text("New local terminal").clicked() {
                        self.actions.push(Action::NewLocal);
                    }
                });
            },
        );

        let Some(terminal) = self.local_tabs.get_mut(self.active_local) else {
            centered_message(ui, body, "No terminal open. Use + to open one.");
            return;
        };
        let view = TerminalView {
            events,
            theme: &self.theme,
            font_size: self.config.font_size,
            focused,
            id: Id::unique("local-terminal"),
        };
        if show_terminal(ui, body, terminal, &view) {
            self.focus = Pane::Local;
        }
    }

    fn sessions_window(&mut self, ctx: &Context) {
        let mut open = self.sessions_open;
        egui::Window::new("SSH Sessions")
            .open(&mut open)
            .default_width(460.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .collapsible(false)
            .show(ctx, |ui| {
                if self.config.sessions.is_empty() {
                    ui.label("No saved sessions yet.");
                }
                let mut connect = None;
                let mut delete = None;
                for (index, session) in self.config.sessions.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button("Connect").clicked() {
                            connect = Some(session.clone());
                        }
                        let user = if session.user.is_empty() {
                            String::new()
                        } else {
                            format!("{}@", session.user)
                        };
                        ui.label(RichText::new(session.display_name()).strong());
                        ui.label(format!("{user}{}:{}", session.host, session.port));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Delete").clicked() {
                                delete = Some(index);
                            }
                            if ui.small_button("Edit").clicked() {
                                self.editor = Some(SessionEditor {
                                    index: Some(index),
                                    draft: session.clone(),
                                    error: None,
                                });
                            }
                        });
                    });
                }
                if let Some(session) = connect {
                    self.actions.push(Action::Connect(session));
                    self.sessions_open = false;
                }
                if let Some(index) = delete {
                    self.config.sessions.remove(index);
                    self.editor = None;
                    self.save_now();
                }
                ui.separator();
                if self.editor.is_none() && ui.button("+ New session").clicked() {
                    self.editor = Some(SessionEditor {
                        index: None,
                        draft: SshSession::default(),
                        error: None,
                    });
                }
                self.session_form(ui);
            });
        // Connecting closes the dialog from inside the closure above.
        self.sessions_open = self.sessions_open && open;
    }

    fn session_form(&mut self, ui: &mut Ui) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        egui::Grid::new("session-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let draft = &mut editor.draft;
                ui.label("Name");
                ui.text_edit_singleline(&mut draft.name);
                ui.end_row();
                ui.label("Host or ssh alias");
                ui.text_edit_singleline(&mut draft.host);
                ui.end_row();
                ui.label("User");
                ui.text_edit_singleline(&mut draft.user);
                ui.end_row();
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut draft.port).range(1..=65535));
                ui.end_row();
                ui.label("Identity file");
                ui.text_edit_singleline(&mut draft.identity_file);
                ui.end_row();
            });
        ui.small("Passwords are not stored: ssh asks for them in the main terminal.");
        if let Some(error) = &editor.error {
            ui.colored_label(self.theme.ansi[1], error);
        }
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            save = ui.button("Save").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            self.editor = None;
        } else if save {
            if let Err(error) = editor.draft.validate() {
                editor.error = Some(error);
                return;
            }
            let draft = editor.draft.clone();
            match editor.index {
                Some(index) if index < self.config.sessions.len() => {
                    self.config.sessions[index] = draft;
                }
                _ => self.config.sessions.push(draft),
            }
            self.editor = None;
            self.save_now();
        }
    }

    fn apply_theme(&mut self, ctx: &Context) {
        self.theme = Theme::named(
            &self.config.theme,
            self.config.foreground_override,
            self.config.background_override,
        );
        apply_visuals(ctx, &self.theme);
    }

    fn settings_window(&mut self, ctx: &Context) {
        let mut open = self.settings_open;
        let mut changed = false;
        let mut theme_changed = false;
        let mut reload_font = false;
        let mut reload_image = false;

        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(440.0)
            .collapsible(false)
            .show(ctx, |ui| {
                let config = &mut self.config;
                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Theme");
                        egui::ComboBox::from_id_salt("theme")
                            .selected_text(config.theme.clone())
                            .show_ui(ui, |ui| {
                                for name in theme::names() {
                                    if ui
                                        .selectable_value(&mut config.theme, name.to_string(), name)
                                        .changed()
                                    {
                                        theme_changed = true;
                                    }
                                }
                            });
                        ui.end_row();

                        theme_changed |= color_override(
                            ui,
                            "Font color",
                            &mut config.foreground_override,
                            self.theme.foreground,
                        );
                        theme_changed |= color_override(
                            ui,
                            "Background color",
                            &mut config.background_override,
                            self.theme.background,
                        );

                        ui.label("Font size");
                        changed |= ui
                            .add(egui::Slider::new(&mut config.font_size, 8.0..=32.0).step_by(0.5))
                            .changed();
                        ui.end_row();

                        ui.label("Font file (.ttf/.otf)");
                        ui.horizontal(|ui| {
                            changed |= ui.text_edit_singleline(&mut config.font_path).changed();
                            reload_font = ui.button("Apply").clicked();
                        });
                        ui.end_row();

                        ui.label("Background image");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .text_edit_singleline(&mut config.background_image)
                                .changed();
                            reload_image = ui.button("Apply").clicked();
                        });
                        ui.end_row();

                        ui.label("Image strength");
                        changed |= ui
                            .add(egui::Slider::new(
                                &mut config.background_image_strength,
                                0.0..=1.0,
                            ))
                            .changed();
                        ui.end_row();

                        ui.label("Shell for new terminals");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut config.local_shell)
                                    .hint_text("login shell"),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Scrollback lines");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut config.scrollback)
                                    .range(0..=200_000)
                                    .speed(100),
                            )
                            .changed();
                        ui.end_row();
                    });
                if let Some(message) = &self.settings_message {
                    ui.colored_label(self.theme.ansi[1], message);
                }
                ui.small(concat!("Version ", env!("CARGO_PKG_VERSION")));
            });
        self.settings_open = open;

        if theme_changed {
            self.apply_theme(ctx);
        }
        if reload_font {
            self.settings_message = install_fonts(ctx, &self.config.font_path);
            changed = true;
        }
        if reload_image {
            match load_background(ctx, &self.config.background_image) {
                Ok(texture) => {
                    self.background = texture;
                    self.settings_message = None;
                }
                Err(err) => self.settings_message = Some(err),
            }
            changed = true;
        }
        if changed || theme_changed {
            self.mark_dirty();
        }
    }

    fn paint_background(&self, ui: &Ui, rect: Rect) {
        ui.painter().rect_filled(rect, 0.0, self.theme.background);
        let Some(texture) = &self.background else {
            return;
        };
        let [width, height] = texture.size().map(|v| v as f32);
        let image_aspect = width / height;
        let rect_aspect = rect.width() / rect.height();
        // Crop to fill the window without stretching.
        let uv = if image_aspect > rect_aspect {
            let w = rect_aspect / image_aspect;
            Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
        } else {
            let h = image_aspect / rect_aspect;
            Rect::from_min_max(pos2(0.0, 0.5 - h / 2.0), pos2(1.0, 0.5 + h / 2.0))
        };
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
    }

    /// Keeps keys like Tab and the arrows out of egui's widget navigation,
    /// unless the user is typing into a text field in a dialog.
    fn claim_keyboard(ui: &mut Ui) -> bool {
        let id = Id::unique(KEYBOARD_ID);
        let anchor = Rect::from_min_size(ui.max_rect().min, vec2(1.0, 1.0));
        let response = ui.interact(anchor, id, Sense::click());
        let focused = ui.memory(|m| m.focused());
        if focused.is_none() {
            response.request_focus();
        }
        let filter = egui::EventFilter {
            tab: true,
            horizontal_arrows: true,
            vertical_arrows: true,
            escape: true,
        };
        ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
        focused.is_none_or(|focused| focused == id)
    }
}

fn color_override(ui: &mut Ui, label: &str, value: &mut Option<[u8; 3]>, current: Color32) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        let mut custom = value.is_some();
        if ui.checkbox(&mut custom, "Custom").changed() {
            *value = custom.then_some([current.r(), current.g(), current.b()]);
            changed = true;
        }
        if let Some(rgb) = value {
            changed |= ui.color_edit_button_srgb(rgb).changed();
        }
    });
    ui.end_row();
    changed
}

fn centered_message(ui: &Ui, rect: Rect, message: &str) {
    let color = ui.visuals().weak_text_color();
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        message,
        egui::FontId::proportional(14.0),
        color,
    );
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        const SAVE_DELAY: Duration = Duration::from_secs(1);

        let ctx = ui.ctx().clone();
        self.poll_terminals(&ctx);

        let has_keyboard = Self::claim_keyboard(ui);
        // A newly focused text field shouldn't also send its keys to a terminal.
        let events = ctx.input(|i| i.events.clone());
        if has_keyboard {
            self.keyboard(&ctx, &events);
        }

        egui::Panel::top("top-bar")
            .frame(
                Frame::NONE
                    .inner_margin(vec2(8.0, 4.0))
                    .fill(self.theme.background),
            )
            .show(ui, |ui| self.top_bar(ui));

        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let full = ui.max_rect();
                self.paint_background(ui, full);
                let area = full.shrink(GAP);

                let split_x = (area.left() + area.width() * self.config.split_x).round();
                let left = Rect::from_min_max(area.min, pos2(split_x - GAP / 2.0, area.bottom()));
                let right = Rect::from_min_max(pos2(split_x + GAP / 2.0, area.top()), area.max);
                let split_y = (right.top() + right.height() * self.config.split_y).round();
                let top = Rect::from_min_max(right.min, pos2(right.right(), split_y - GAP / 2.0));
                let bottom = Rect::from_min_max(pos2(right.left(), split_y + GAP / 2.0), right.max);

                self.main_pane(ui, left, &events);
                self.btop_pane(ui, top, &events);
                self.local_pane(ui, bottom, &events);

                let vertical = Rect::from_min_max(
                    pos2(left.right(), area.top()),
                    pos2(right.left(), area.bottom()),
                );
                let horizontal = Rect::from_min_max(
                    pos2(right.left(), top.bottom()),
                    pos2(right.right(), bottom.top()),
                );
                self.divider(ui, vertical, "split-x", true, area);
                self.divider(ui, horizontal, "split-y", false, right);
            });

        if self.sessions_open {
            self.sessions_window(&ctx);
        }
        if self.settings_open {
            self.settings_window(&ctx);
        }
        self.run_actions(&ctx);

        if let Some(since) = self.dirty_since {
            if since.elapsed() >= SAVE_DELAY {
                self.save_now();
            } else {
                ctx.request_repaint_after(SAVE_DELAY);
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty_since.is_some() {
            self.save_now();
        }
        for tab in &mut self.main_tabs {
            // Done here, synchronously: threads wouldn't outlive the process.
            if let Some(target) = tab.target.take() {
                target.close_master();
            }
        }
    }
}
