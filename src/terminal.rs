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
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{self, ClipboardType, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Rgb;
use egui::{Context, Event, MouseWheelUnit, PointerButton, Pos2, Rect};

use crate::input;
use crate::links;
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

/// A link on screen, from `start` to `end` inclusive, in grid coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub uri: String,
    pub start: Point,
    pub end: Point,
}

impl Link {
    pub fn contains(&self, point: Point) -> bool {
        self.start <= point && point <= self.end
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
    /// The link under the pointer, underlined while hovered.
    pub hovered_link: Option<Link>,
    /// A link the user clicked, waiting to be opened.
    link_request: Option<String>,
    /// When autoscroll last ran, while a selection is dragged past an edge.
    autoscroll_time: Option<f64>,
    autoscroll_remainder: f32,
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
            hovered_link: None,
            link_request: None,
            autoscroll_time: None,
            autoscroll_remainder: 0.0,
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

    /// The grid point shown at a screen cell, which accounts for scrollback.
    fn grid_point(&self, col: usize, row: usize) -> Point {
        let offset = self.term.lock().grid().display_offset() as i32;
        Point::new(Line(row as i32 - offset), Column(col))
    }

    /// The link at `point`: an explicit hyperlink set by the program, or a
    /// web address in the text, which may wrap onto following lines.
    fn link_at(&self, point: Point) -> Option<Link> {
        const MAX_WRAPPED_ROWS: i32 = 16;
        let term = self.term.lock();
        let grid = term.grid();
        if point.line < grid.topmost_line() || point.line > grid.bottommost_line() {
            return None;
        }
        let last_col = grid.last_column();

        if let Some(hyperlink) = grid[point].hyperlink() {
            if !links::is_openable(hyperlink.uri()) {
                return None;
            }
            let same = |p: Point| grid[p].hyperlink().as_ref() == Some(&hyperlink);
            let mut start = point;
            while start.column.0 > 0 && same(Point::new(start.line, Column(start.column.0 - 1))) {
                start.column.0 -= 1;
            }
            let mut end = point;
            while end.column < last_col && same(Point::new(end.line, Column(end.column.0 + 1))) {
                end.column.0 += 1;
            }
            return Some(Link {
                uri: hyperlink.uri().to_string(),
                start,
                end,
            });
        }

        let wraps = |line: i32| grid[Line(line)][last_col].flags.contains(Flags::WRAPLINE);
        let (row, top, bottom) = (
            point.line.0,
            grid.topmost_line().0,
            grid.bottommost_line().0,
        );
        let mut first = row;
        while first > top && row - first < MAX_WRAPPED_ROWS && wraps(first - 1) {
            first -= 1;
        }
        let mut last = row;
        while last < bottom && last - row < MAX_WRAPPED_ROWS && wraps(last) {
            last += 1;
        }
        let mut chars = Vec::new();
        let mut points = Vec::new();
        for line in first..=last {
            for col in 0..=last_col.0 {
                let p = Point::new(Line(line), Column(col));
                let cell = &grid[p];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                chars.push(cell.c);
                points.push(p);
            }
        }
        links::find_urls(&chars)
            .into_iter()
            .map(|(start, end)| {
                let mut end_point = points[end - 1];
                if grid[end_point].flags.contains(Flags::WIDE_CHAR) {
                    end_point.column.0 += 1;
                }
                Link {
                    uri: chars[start..end].iter().collect(),
                    start: points[start],
                    end: end_point,
                }
            })
            .find(|link| link.contains(point))
    }

    /// Finds the link under the pointer, if any. `pos` is `None` when the
    /// pointer isn't over this terminal.
    pub fn update_hover(&mut self, pos: Option<Pos2>, rect: Rect, cell: CellSize) {
        self.hovered_link = match pos {
            Some(pos) if !self.selecting && rect.contains(pos) => {
                let (col, row, _) = self.cell_at(pos, rect, cell);
                self.link_at(self.grid_point(col, row))
            }
            _ => None,
        };
    }

    /// The link the user clicked since the last call.
    pub fn take_link_request(&mut self) -> Option<String> {
        self.link_request.take()
    }

    /// Scrolls while a selection is dragged above or below the terminal, so
    /// it can grow past what fits on screen. Returns true while scrolling,
    /// since it has to keep going even when the pointer stops moving.
    pub fn autoscroll_selection(
        &mut self,
        pointer: Option<Pos2>,
        rect: Rect,
        cell: CellSize,
        now: f64,
    ) -> bool {
        const BASE_SPEED: f32 = 8.0;
        const SPEED_PER_ROW: f32 = 12.0;
        const MAX_SPEED: f32 = 400.0;

        let past_edge = pointer.filter(|_| self.selecting).and_then(|pos| {
            if pos.y < rect.top() {
                Some((pos, rect.top() - pos.y))
            } else if pos.y > rect.bottom() {
                Some((pos, rect.bottom() - pos.y))
            } else {
                None
            }
        });
        let Some((pos, distance)) = past_edge else {
            self.autoscroll_time = None;
            self.autoscroll_remainder = 0.0;
            return false;
        };

        // Lines per second grow with how far past the edge the pointer is.
        // Positive distances scroll up, into the scrollback.
        let rows_past = distance.abs() / cell.height;
        let speed = (BASE_SPEED + SPEED_PER_ROW * rows_past).min(MAX_SPEED);
        let last = self.autoscroll_time.replace(now).unwrap_or(now);
        let elapsed = (now - last).clamp(0.0, 0.1) as f32;
        self.autoscroll_remainder += speed * elapsed * distance.signum();
        let lines = self.autoscroll_remainder.trunc();
        self.autoscroll_remainder -= lines;
        if lines != 0.0 {
            self.term.lock().scroll_display(Scroll::Delta(lines as i32));
        }
        // The pointer is outside, so this lands on the top or bottom row.
        let (col, row, side) = self.cell_at(pos, rect, cell);
        self.update_selection(col, row, side);
        true
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
                        // Ctrl+click, or Cmd+click on macOS, opens a link.
                        if code == 0
                            && modifiers.command
                            && let Some(link) = self.link_at(self.grid_point(col, row))
                        {
                            self.link_request = Some(link.uri);
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

    fn point(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    #[test]
    fn finds_link_under_the_pointer() {
        let terminal = run_until_exit("printf 'go to https://example.com/x now'");
        let link = terminal.link_at(point(0, 10)).unwrap();
        assert_eq!(link.uri, "https://example.com/x");
        assert_eq!((link.start, link.end), (point(0, 6), point(0, 26)));
        assert!(terminal.link_at(point(0, 2)).is_none());
    }

    #[test]
    fn finds_link_that_wraps_onto_the_next_line() {
        // The terminal is 80 columns wide, so this wraps after "https://".
        let script = format!("printf '{}https://example.com/long'", "a ".repeat(36));
        let terminal = run_until_exit(&script);
        let link = terminal.link_at(point(1, 3)).unwrap();
        assert_eq!(link.uri, "https://example.com/long");
        assert_eq!(link.start, point(0, 72));
    }

    #[test]
    fn finds_explicit_hyperlinks() {
        let terminal =
            run_until_exit("printf '\\033]8;;https://example.com\\033\\\\click\\033]8;;\\033\\\\'");
        let link = terminal.link_at(point(0, 2)).unwrap();
        assert_eq!(link.uri, "https://example.com");
        assert_eq!((link.start, link.end), (point(0, 0), point(0, 4)));
    }

    #[test]
    fn output_reaches_the_grid() {
        let terminal = run_until_exit("printf hello-omega");
        let term = terminal.term.lock();
        let text: String = term.grid().display_iter().map(|c| c.c).take(11).collect();
        assert_eq!(text, "hello-omega");
    }
}
