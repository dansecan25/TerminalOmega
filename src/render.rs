//! Paints a terminal grid with egui.

use alacritty_terminal::index::Point;
use alacritty_terminal::term::RenderableContent;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{Color32, Context, FontFamily, FontId, Painter, Pos2, Rect, Shape, pos2, vec2};

use crate::terminal::Terminal;
use crate::theme::Theme;

pub const FONT_REGULAR: &str = "terminal-regular";
pub const FONT_BOLD: &str = "terminal-bold";

pub fn font_id(size: f32, bold: bool) -> FontId {
    let name = if bold { FONT_BOLD } else { FONT_REGULAR };
    FontId::new(size, FontFamily::Name(name.into()))
}

#[derive(Clone, Copy, Debug)]
pub struct CellSize {
    pub width: f32,
    pub height: f32,
}

pub fn cell_size(ctx: &Context, size: f32) -> CellSize {
    let font = font_id(size, false);
    let (width, height) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let ppp = ctx.pixels_per_point();
    CellSize {
        width,
        height: (height * ppp).ceil() / ppp,
    }
}

/// Largest font size, up to `preferred`, that fits `cols`x`rows` into the area.
pub fn fit_font_size(ctx: &Context, preferred: f32, area: egui::Vec2, cols: f32, rows: f32) -> f32 {
    const MIN_SIZE: f32 = 6.0;
    let cell = cell_size(ctx, preferred);
    let scale = (area.x / (cols * cell.width)).min(area.y / (rows * cell.height));
    if scale >= 1.0 {
        return preferred;
    }
    // Row heights round up to whole pixels, so step down until it really fits.
    let mut size = (preferred * scale).max(MIN_SIZE);
    while size > MIN_SIZE {
        let cell = cell_size(ctx, size);
        if cols * cell.width <= area.x && rows * cell.height <= area.y {
            break;
        }
        size = (size - 0.5).max(MIN_SIZE);
    }
    size
}

fn resolve(color: Color, colors: &Colors, theme: &Theme) -> Color32 {
    let index = match color {
        Color::Spec(rgb) => return Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Named(named) => named as usize,
        Color::Indexed(index) => index as usize,
    };
    match colors[index] {
        Some(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        None => theme.color(index),
    }
}

/// Weights of the four arms of a box-drawing character: left, right, up, down.
fn box_arms(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [1, 1, 0, 0],
        '━' | '═' => [2, 2, 0, 0],
        '│' => [0, 0, 1, 1],
        '┃' | '║' => [0, 0, 2, 2],
        '┌' => [0, 1, 0, 1],
        '┐' => [1, 0, 0, 1],
        '└' => [0, 1, 1, 0],
        '┘' => [1, 0, 1, 0],
        '├' => [0, 1, 1, 1],
        '┤' => [1, 0, 1, 1],
        '┬' => [1, 1, 0, 1],
        '┴' => [1, 1, 1, 0],
        '┼' => [1, 1, 1, 1],
        '┏' | '╔' => [0, 2, 0, 2],
        '┓' | '╗' => [2, 0, 0, 2],
        '┗' | '╚' => [0, 2, 2, 0],
        '┛' | '╝' => [2, 0, 2, 0],
        '┣' | '╠' => [0, 2, 2, 2],
        '┫' | '╣' => [2, 0, 2, 2],
        '┳' | '╦' => [2, 2, 0, 2],
        '┻' | '╩' => [2, 2, 2, 0],
        '╋' | '╬' => [2, 2, 2, 2],
        _ => return None,
    })
}

struct Snap(f32);

impl Snap {
    fn at(&self, v: f32) -> f32 {
        (v * self.0).round() / self.0
    }
    fn pixel(&self) -> f32 {
        1.0 / self.0
    }
}

fn draw_box(mesh: &mut Mesh, arms: [u8; 4], r: Rect, color: Color32, snap: &Snap) {
    let thin = snap.at((r.height() * 0.07).max(snap.pixel()));
    let cx = snap.at(r.center().x);
    let cy = snap.at(r.center().y);
    let half = |weight: u8| thin * weight as f32 / 2.0;
    let [left, right, up, down] = arms;
    // Arms overlap in the middle so corners have no notch.
    let h = half(left.max(right)).max(half(up.max(down)));
    if left > 0 {
        let w = half(left);
        mesh.add_colored_rect(
            Rect::from_min_max(pos2(r.left(), cy - w), pos2(cx + h, cy + w)),
            color,
        );
    }
    if right > 0 {
        let w = half(right);
        mesh.add_colored_rect(
            Rect::from_min_max(pos2(cx - h, cy - w), pos2(r.right(), cy + w)),
            color,
        );
    }
    if up > 0 {
        let w = half(up);
        mesh.add_colored_rect(
            Rect::from_min_max(pos2(cx - w, r.top()), pos2(cx + w, cy + h)),
            color,
        );
    }
    if down > 0 {
        let w = half(down);
        mesh.add_colored_rect(
            Rect::from_min_max(pos2(cx - w, cy - h), pos2(cx + w, r.bottom())),
            color,
        );
    }
}

/// Rounded corners, as a curve from the middle of one edge to another.
fn rounded_corner(c: char, r: Rect, color: Color32, snap: &Snap, shapes: &mut Vec<Shape>) -> bool {
    let (cx, cy) = (snap.at(r.center().x), snap.at(r.center().y));
    let (start, end) = match c {
        '╭' => (pos2(r.right(), cy), pos2(cx, r.bottom())),
        '╮' => (pos2(r.left(), cy), pos2(cx, r.bottom())),
        '╯' => (pos2(r.left(), cy), pos2(cx, r.top())),
        '╰' => (pos2(r.right(), cy), pos2(cx, r.top())),
        _ => return false,
    };
    let control = pos2(cx, cy);
    let bezier = |t: f32| {
        let u = 1.0 - t;
        Pos2::new(
            u * u * start.x + 2.0 * u * t * control.x + t * t * end.x,
            u * u * start.y + 2.0 * u * t * control.y + t * t * end.y,
        )
    };
    let points = (0..=8).map(|i| bezier(i as f32 / 8.0)).collect();
    let width = (r.height() * 0.07).max(snap.pixel());
    shapes.push(Shape::Path(PathShape::line(
        points,
        PathStroke::new(width, color),
    )));
    true
}

fn draw_block(mesh: &mut Mesh, c: char, r: Rect, color: Color32, snap: &Snap) -> bool {
    let (w, h) = (r.width(), r.height());
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_min_max(
            pos2(snap.at(r.left() + w * x0), snap.at(r.top() + h * y0)),
            pos2(snap.at(r.left() + w * x1), snap.at(r.top() + h * y1)),
        )
    };
    match c {
        '█' => mesh.add_colored_rect(r, color),
        '▀' => mesh.add_colored_rect(rect(0.0, 0.0, 1.0, 0.5), color),
        '▐' => mesh.add_colored_rect(rect(0.5, 0.0, 1.0, 1.0), color),
        '▁'..='▇' => {
            let eighths = c as u32 - 0x2580;
            mesh.add_colored_rect(rect(0.0, 1.0 - eighths as f32 / 8.0, 1.0, 1.0), color);
        }
        '▉'..='▏' => {
            let eighths = 0x2590 - c as u32;
            mesh.add_colored_rect(rect(0.0, 0.0, eighths as f32 / 8.0, 1.0), color);
        }
        '░' => mesh.add_colored_rect(r, color.gamma_multiply(0.25)),
        '▒' => mesh.add_colored_rect(r, color.gamma_multiply(0.5)),
        '▓' => mesh.add_colored_rect(r, color.gamma_multiply(0.75)),
        _ => return false,
    }
    true
}

fn draw_braille(mesh: &mut Mesh, c: char, r: Rect, color: Color32) -> bool {
    if !('\u{2800}'..='\u{28FF}').contains(&c) {
        return false;
    }
    // Bit index for each dot, by column then row.
    const DOTS: [[u32; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];
    let bits = c as u32 - 0x2800;
    let (cell_w, cell_h) = (r.width() / 2.0, r.height() / 4.0);
    let size = cell_w.min(cell_h) * 0.72;
    for (col, rows) in DOTS.iter().enumerate() {
        for (row, bit) in rows.iter().enumerate() {
            if bits & (1 << bit) != 0 {
                let center = r.min + vec2(cell_w * (col as f32 + 0.5), cell_h * (row as f32 + 0.5));
                mesh.add_colored_rect(Rect::from_center_size(center, vec2(size, size)), color);
            }
        }
    }
    true
}

pub struct PaintOptions<'a> {
    pub theme: &'a Theme,
    pub font_size: f32,
    pub cell: CellSize,
    pub focused: bool,
}

pub fn paint(painter: &Painter, origin: Pos2, terminal: &Terminal, options: &PaintOptions<'_>) {
    let PaintOptions {
        theme,
        font_size,
        cell,
        focused,
    } = *options;
    let snap = Snap(painter.ctx().pixels_per_point());
    let term = terminal.term.lock();
    let RenderableContent {
        display_iter,
        selection,
        cursor,
        display_offset,
        colors,
        ..
    } = term.renderable_content();

    let cell_rect = |col: usize, row: usize, span: usize| {
        Rect::from_min_max(
            pos2(
                snap.at(origin.x + col as f32 * cell.width),
                snap.at(origin.y + row as f32 * cell.height),
            ),
            pos2(
                snap.at(origin.x + (col + span) as f32 * cell.width),
                snap.at(origin.y + (row + 1) as f32 * cell.height),
            ),
        )
    };

    let cursor_point: Option<Point> = (cursor.shape != CursorShape::Hidden).then_some(cursor.point);
    let mut background = Mesh::default();
    let mut foreground = Mesh::default();
    let mut curves = Vec::new();
    let mut glyphs = Vec::new();
    let regular = font_id(font_size, false);
    let bold = font_id(font_size, true);

    painter.ctx().fonts_mut(|fonts| {
        for indexed in display_iter {
            let flags = indexed.cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let point = indexed.point;
            let row = (point.line.0 + display_offset as i32) as usize;
            let col = point.column.0;
            let span = if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let rect = cell_rect(col, row, span);

            let mut fg = resolve(indexed.cell.fg, colors, theme);
            let mut bg = resolve(indexed.cell.bg, colors, theme);
            let mut has_bg = indexed.cell.bg != Color::Named(NamedColor::Background);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
                has_bg = true;
            }
            if flags.contains(Flags::DIM) {
                fg = fg.gamma_multiply(0.66);
            }
            if flags.contains(Flags::HIDDEN) {
                fg = Color32::TRANSPARENT;
            }
            if selection.as_ref().is_some_and(|s| s.contains(point)) {
                bg = theme.selection;
                has_bg = true;
            }
            if focused && cursor.shape == CursorShape::Block && cursor_point == Some(point) {
                bg = theme.cursor;
                fg = theme.background;
                has_bg = true;
            }
            if has_bg {
                background.add_colored_rect(rect, bg);
            }

            if flags.intersects(Flags::ALL_UNDERLINES) {
                let y = rect.bottom() - snap.pixel();
                let line =
                    Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom()));
                foreground.add_colored_rect(line, fg);
            }
            if flags.contains(Flags::STRIKEOUT) {
                let y = snap.at(rect.center().y);
                let line =
                    Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + snap.pixel()));
                foreground.add_colored_rect(line, fg);
            }

            let c = indexed.cell.c;
            if c == ' ' || c == '\0' {
                continue;
            }
            if let Some(arms) = box_arms(c) {
                draw_box(&mut foreground, arms, rect, fg, &snap);
                continue;
            }
            if rounded_corner(c, rect, fg, &snap, &mut curves)
                || draw_block(&mut foreground, c, rect, fg, &snap)
                || draw_braille(&mut foreground, c, rect, fg)
            {
                continue;
            }

            let mut text = String::from(c);
            if let Some(extra) = indexed.cell.zerowidth() {
                text.extend(extra);
            }
            let font = if flags.contains(Flags::BOLD) {
                &bold
            } else {
                &regular
            };
            let galley = fonts.layout_no_wrap(text, font.clone(), fg);
            let offset = ((rect.height() - galley.size().y) / 2.0).max(0.0);
            let pos = pos2(rect.left(), snap.at(rect.top() + offset));
            glyphs.push(Shape::galley(pos, galley, fg));
        }
    });

    painter.add(Shape::mesh(background));
    painter.extend(curves);
    painter.add(Shape::mesh(foreground));
    painter.extend(glyphs);

    if let Some(point) = cursor_point {
        let row = (point.line.0 + display_offset as i32) as usize;
        let r = cell_rect(point.column.0, row, 1);
        let beam = 2.0 * snap.pixel();
        let stroke = egui::Stroke::new(snap.pixel(), theme.cursor);
        match cursor.shape {
            _ if !focused => {
                painter.rect_stroke(
                    r.shrink(snap.pixel() / 2.0),
                    0.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
            CursorShape::Beam => {
                painter.rect_filled(
                    Rect::from_min_size(r.min, vec2(beam, r.height())),
                    0.0,
                    theme.cursor,
                );
            }
            CursorShape::Underline => {
                let top = pos2(r.left(), r.bottom() - beam);
                painter.rect_filled(
                    Rect::from_min_size(top, vec2(r.width(), beam)),
                    0.0,
                    theme.cursor,
                );
            }
            CursorShape::HollowBlock => {
                painter.rect_stroke(
                    r.shrink(snap.pixel() / 2.0),
                    0.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
            _ => {}
        }
    }
}
