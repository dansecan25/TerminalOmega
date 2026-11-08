//! One terminal: a PTY, its emulator state, and its mouse/scroll handling.

use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{self, ClipboardType, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Rgb;
use egui::{Context, Event, MouseWheelUnit, PointerButton, Pos2, Rect};

use crate::input;
use crate::render::CellSize;
use crate::theme::Theme;

#[derive(Clone)]
pub struct Listener {
    events: Sender<TermEvent>,
    ctx: Context,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

/// What to launch inside the terminal.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// `None` runs the user's login shell.
    pub program: Option<(String, Vec<String>)>,
}

impl Launch {
    pub fn shell(configured: &str) -> Self {
        let configured = configured.trim();
        let program = (!configured.is_empty()).then(|| (configured.to_string(), Vec::new()));
        Self { program }
    }

    pub fn command(program: &str, args: Vec<String>) -> Self {
        Self {
            program: Some((program.to_string(), args)),
        }
    }
}

pub struct Terminal {
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    events: Receiver<TermEvent>,
    /// Set by the running program through an escape sequence.
    pub title: Option<String>,
    pub exit: Option<ExitStatus>,
    size: (usize, usize),
    selecting: bool,
    held_button: Option<u8>,
    last_cell: Option<(usize, usize)>,
    scroll_remainder: f32,
    last_click: Option<(f64, u8)>,
    was_focused: bool,
}

const INITIAL_SIZE: (usize, usize) = (80, 24);

impl Terminal {
    pub fn spawn(ctx: &Context, launch: Launch, scrollback: usize) -> Result<Self, String> {
        let (tx, events) = channel();
        let listener = Listener {
            events: tx,
            ctx: ctx.clone(),
        };
        let config = term::Config {
            scrolling_history: scrollback,
            ..Default::default()
        };
        let (cols, rows) = INITIAL_SIZE;
        let term = Term::new(config, &TermSize::new(cols, rows), listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let mut env = HashMap::from([
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
        ]);
        // Apps opened from Finder get no locale, which breaks UTF-8 programs.
        if cfg!(target_os = "macos") && std::env::var_os("LANG").is_none() {
            env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        }

        let options = tty::Options {
            shell: launch
                .program
                .map(|(program, args)| tty::Shell::new(program, args)),
            drain_on_exit: true,
            env,
            ..Default::default()
        };
        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: 8,
            cell_height: 16,
        };
        let pty = tty::new(&options, window_size, 0).map_err(|e| e.to_string())?;
        let event_loop =
            EventLoop::new(term.clone(), listener, pty, true, false).map_err(|e| e.to_string())?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(Self {
            term,
            sender,
            events,
            title: None,
            exit: None,
            size: INITIAL_SIZE,
            selecting: false,
            held_button: None,
            last_cell: None,
            scroll_remainder: 0.0,
            last_click: None,
            was_focused: false,
        })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes.into()));
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    /// Handles requests from the emulator. Terminals in hidden tabs are polled
    /// too, because programs block waiting for answers to their queries.
    pub fn poll(&mut self, ctx: &Context, theme: &Theme) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
                TermEvent::Title(title) => self.title = Some(title),
                TermEvent::ResetTitle => self.title = None,
                TermEvent::ClipboardStore(ClipboardType::Clipboard, text) => ctx.copy_text(text),
                TermEvent::ColorRequest(index, format) => {
                    let override_color = self.term.lock().colors()[index];
                    let color = override_color.unwrap_or_else(|| {
                        let c = theme.color(index);
                        Rgb {
                            r: c.r(),
                            g: c.g(),
                            b: c.b(),
                        }
                    });
                    self.write(format(color).into_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let (cols, rows) = self.size;
                    let size = WindowSize {
                        num_lines: rows as u16,
                        num_cols: cols as u16,
                        cell_width: 8,
                        cell_height: 16,
                    };
                    self.write(format(size).into_bytes());
                }
                TermEvent::ChildExit(status) => self.exit = Some(status),
                // Reading the clipboard is refused on purpose: a remote host
                // could otherwise silently read whatever the user last copied.
                _ => {}
            }
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell: CellSize) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if self.size == (cols, rows) {
            return;
        }
        self.size = (cols, rows);
        self.term.lock().resize(TermSize::new(cols, rows));
        let _ = self.sender.send(Msg::Resize(WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell.width as u16,
            cell_height: cell.height as u16,
        }));
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.write(input::paste_bytes(text, bracketed));
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    /// The last non-empty lines on screen.
    pub fn last_output(&self, max_lines: usize) -> String {
        let term = self.term.lock();
        let cells: Vec<char> = term.grid().display_iter().map(|cell| cell.c).collect();
        let lines: Vec<String> = cells
            .chunks(term.columns().max(1))
            .map(|line| line.iter().collect::<String>().trim_end().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        lines[lines.len().saturating_sub(max_lines)..].join("\n")
    }

    /// Jumps back to the live screen after the user types while scrolled up.
    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
    }

    /// Tells programs that asked for it when this pane gains or loses focus.
    pub fn set_focused(&mut self, focused: bool) {
        if focused != self.was_focused {
            self.was_focused = focused;
            if self.mode().contains(TermMode::FOCUS_IN_OUT) {
                self.write(if focused {
                    b"\x1b[I".to_vec()
                } else {
                    b"\x1b[O".to_vec()
                });
            }
        }
    }

    fn cell_at(&self, pos: Pos2, rect: Rect, cell: CellSize) -> (usize, usize, Side) {
        let (cols, rows) = self.size;
        let x = ((pos.x - rect.left()) / cell.width).max(0.0);
        let y = ((pos.y - rect.top()) / cell.height).max(0.0);
        let col = (x as usize).min(cols - 1);
        let row = (y as usize).min(rows - 1);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        (col, row, side)
    }

    fn report_mouse(&self, code: u8, col: usize, row: usize, pressed: bool, mode: TermMode) {
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        if let Some(bytes) = input::mouse_report(code, col, row, pressed, sgr) {
            self.write(bytes);
        }
    }

    fn start_selection(&mut self, col: usize, row: usize, side: Side, now: f64) {
        const MULTI_CLICK_SECONDS: f64 = 0.4;
        let clicks = match self.last_click {
            Some((time, count)) if now - time < MULTI_CLICK_SECONDS => count % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((now, clicks));
        let kind = match clicks {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row as i32 - offset), Column(col));
        term.selection = Some(Selection::new(kind, point, side));
        drop(term);
        self.selecting = true;
    }

    fn update_selection(&self, col: usize, row: usize, side: Side) {
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row as i32 - offset), Column(col));
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    fn scroll(&mut self, lines: i32, col: usize, row: usize, mods: egui::Modifiers) {
        let mode = self.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !mods.shift {
            let code = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.abs() {
                self.report_mouse(code, col, row, true, mode);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let key = if lines > 0 {
                egui::Key::ArrowUp
            } else {
                egui::Key::ArrowDown
            };
            let app = mode.contains(TermMode::APP_CURSOR);
            if let Some(bytes) = input::key_bytes(key, egui::Modifiers::NONE, app) {
                self.write(bytes.repeat(lines.unsigned_abs() as usize));
            }
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
        }
    }

    /// Processes this frame's pointer events. `hovered` is false when another
    /// window covers the terminal; drags that started here still continue.
    pub fn handle_pointer(
        &mut self,
        events: &[Event],
        rect: Rect,
        hovered: bool,
        cell: CellSize,
        hover_pos: Option<Pos2>,
        now: f64,
    ) {
        for event in events {
            let mode = self.mode();
            let wants_mouse = mode.intersects(TermMode::MOUSE_MODE);
            match event {
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let code = match button {
                        PointerButton::Primary => 0,
                        PointerButton::Middle => 1,
                        PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let (col, row, side) = self.cell_at(*pos, rect, cell);
                    if *pressed {
                        if !hovered {
                            continue;
                        }
                        if wants_mouse && !modifiers.shift {
                            self.held_button = Some(code);
                            self.report_mouse(code | mouse_mods(*modifiers), col, row, true, mode);
                        } else if code == 0 {
                            self.start_selection(col, row, side, now);
                        }
                    } else {
                        if self.held_button == Some(code) {
                            self.held_button = None;
                            self.report_mouse(code | mouse_mods(*modifiers), col, row, false, mode);
                        }
                        if code == 0 {
                            self.selecting = false;
                        }
                    }
                }
                Event::PointerMoved(pos) => {
                    let (col, row, side) = self.cell_at(*pos, rect, cell);
                    if self.selecting {
                        self.update_selection(col, row, side);
                    }
                    if wants_mouse && self.last_cell != Some((col, row)) {
                        let report = match self.held_button {
                            Some(code)
                                if mode
                                    .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) =>
                            {
                                Some(code + 32)
                            }
                            None if mode.contains(TermMode::MOUSE_MOTION) && hovered => Some(35),
                            _ => None,
                        };
                        if let Some(code) = report {
                            self.report_mouse(code, col, row, true, mode);
                        }
                        self.last_cell = Some((col, row));
                    }
                }
                Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } if hovered => {
                    let lines = match unit {
                        MouseWheelUnit::Line => delta.y,
                        MouseWheelUnit::Point => delta.y / cell.height,
                        MouseWheelUnit::Page => delta.y * self.size.1 as f32,
                    };
                    self.scroll_remainder += lines;
                    let whole = self.scroll_remainder.trunc();
                    self.scroll_remainder -= whole;
                    if whole != 0.0 {
                        let pos = hover_pos.unwrap_or(rect.left_top());
                        let (col, row, _) = self.cell_at(pos, rect, cell);
                        self.scroll(whole as i32, col, row, *modifiers);
                    }
                }
                _ => {}
            }
        }
    }
}

fn mouse_mods(mods: egui::Modifiers) -> u8 {
    4 * u8::from(mods.shift) + 8 * u8::from(mods.alt) + 16 * u8::from(mods.ctrl)
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Stops the PTY thread, which closes the PTY and hangs up the child.
        let _ = self.sender.send(Msg::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn run_until_exit(script: &str) -> Terminal {
        let ctx = Context::default();
        let theme = Theme::named("Omega", None, None);
        let launch = Launch::command("sh", vec!["-c".into(), script.into()]);
        let mut terminal = Terminal::spawn(&ctx, launch, 100).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while terminal.exit.is_none() {
            assert!(Instant::now() < deadline, "child did not exit");
            terminal.poll(&ctx, &theme);
            std::thread::sleep(Duration::from_millis(10));
        }
        terminal
    }

    #[test]
    fn exit_code_is_reported() {
        let terminal = run_until_exit("exit 127");
        assert_eq!(terminal.exit.unwrap().code(), Some(127));
    }

    #[test]
    fn last_output_skips_blank_lines() {
        let terminal = run_until_exit("printf 'one\\r\\n\\r\\ntwo\\r\\nthree'");
        assert_eq!(terminal.last_output(2), "two\nthree");
    }

    #[test]
    fn output_reaches_the_grid() {
        let terminal = run_until_exit("printf hello-omega");
        let term = terminal.term.lock();
        let text: String = term.grid().display_iter().map(|c| c.c).take(11).collect();
        assert_eq!(text, "hello-omega");
    }
}
