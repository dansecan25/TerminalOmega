//! Translates key presses into the byte sequences terminals expect.

use egui::{Key, Modifiers};

/// The xterm modifier parameter, or `None` when no modifier is held.
fn modifier_param(mods: Modifiers) -> Option<u8> {
    let value = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
    (value > 1).then_some(value)
}

fn with_alt(mods: Modifiers, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if mods.alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

/// `ESC [ <final>`, `ESC O <final>` in application mode, `ESC [ 1 ; m <final>` with modifiers.
fn letter_seq(letter: u8, mods: Modifiers, application: bool) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[1;{m}{}", letter as char).into_bytes(),
        None if application => vec![0x1b, b'O', letter],
        None => vec![0x1b, b'[', letter],
    }
}

/// `ESC [ <code> ~` or `ESC [ <code> ; m ~` with modifiers.
fn tilde_seq(code: u8, mods: Modifiers) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[{code};{m}~").into_bytes(),
        None => format!("\x1b[{code}~").into_bytes(),
    }
}

/// `ESC O <final>` or `ESC [ 1 ; m <final>` with modifiers (F1-F4).
fn ss3_seq(letter: u8, mods: Modifiers) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[1;{m}{}", letter as char).into_bytes(),
        None => vec![0x1b, b'O', letter],
    }
}

fn control_byte(key: Key) -> Option<u8> {
    let name = key.name();
    if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
        return Some(name.as_bytes()[0] - b'A' + 1);
    }
    match key {
        Key::Space | Key::Num2 => Some(0x00),
        Key::OpenBracket | Key::Num3 => Some(0x1b),
        Key::Backslash | Key::Num4 => Some(0x1c),
        Key::CloseBracket | Key::Num5 => Some(0x1d),
        Key::Num6 => Some(0x1e),
        Key::Slash | Key::Minus | Key::Num7 => Some(0x1f),
        Key::Num8 => Some(0x7f),
        _ => None,
    }
}

/// The bytes for a key press, or `None` if it produces no terminal input.
/// Printable characters arrive separately as text events.
pub fn key_bytes(key: Key, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let bytes = match key {
        Key::ArrowUp => letter_seq(b'A', mods, app_cursor),
        Key::ArrowDown => letter_seq(b'B', mods, app_cursor),
        Key::ArrowRight => letter_seq(b'C', mods, app_cursor),
        Key::ArrowLeft => letter_seq(b'D', mods, app_cursor),
        Key::Home => letter_seq(b'H', mods, app_cursor),
        Key::End => letter_seq(b'F', mods, app_cursor),
        Key::Insert => tilde_seq(2, mods),
        Key::Delete => tilde_seq(3, mods),
        Key::PageUp => tilde_seq(5, mods),
        Key::PageDown => tilde_seq(6, mods),
        Key::F1 => ss3_seq(b'P', mods),
        Key::F2 => ss3_seq(b'Q', mods),
        Key::F3 => ss3_seq(b'R', mods),
        Key::F4 => ss3_seq(b'S', mods),
        Key::F5 => tilde_seq(15, mods),
        Key::F6 => tilde_seq(17, mods),
        Key::F7 => tilde_seq(18, mods),
        Key::F8 => tilde_seq(19, mods),
        Key::F9 => tilde_seq(20, mods),
        Key::F10 => tilde_seq(21, mods),
        Key::F11 => tilde_seq(23, mods),
        Key::F12 => tilde_seq(24, mods),
        Key::Enter => with_alt(mods, b"\r"),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => with_alt(mods, b"\t"),
        Key::Backspace if mods.ctrl => with_alt(mods, b"\x08"),
        Key::Backspace => with_alt(mods, b"\x7f"),
        Key::Escape => with_alt(mods, b"\x1b"),
        _ if mods.ctrl => with_alt(mods, &[control_byte(key)?]),
        _ => return None,
    };
    Some(bytes)
}

/// Prepares clipboard text for sending to the terminal. Escape characters are
/// removed so pasted text can't end bracketed paste early and run commands.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let clean: String = text.chars().filter(|c| *c != '\x1b').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Encodes a mouse report. `button` already includes motion and modifier bits.
pub fn mouse_report(
    button: u8,
    col: usize,
    row: usize,
    pressed: bool,
    sgr: bool,
) -> Option<Vec<u8>> {
    if sgr {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{button};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }
    // The legacy encoding only has one byte per coordinate.
    if col > 222 || row > 222 {
        return None;
    }
    let code = if pressed { button } else { 3 | (button & !3) };
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + code,
        33 + col as u8,
        33 + row as u8,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;
    const CTRL: Modifiers = Modifiers::CTRL;
    const SHIFT: Modifiers = Modifiers::SHIFT;

    #[test]
    fn arrows_respect_application_mode() {
        assert_eq!(key_bytes(Key::ArrowUp, NONE, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, NONE, true).unwrap(), b"\x1bOA");
    }

    #[test]
    fn modified_arrow() {
        assert_eq!(
            key_bytes(Key::ArrowRight, CTRL, false).unwrap(),
            b"\x1b[1;5C"
        );
    }

    #[test]
    fn control_letters() {
        assert_eq!(key_bytes(Key::A, CTRL, false).unwrap(), [1]);
        assert_eq!(key_bytes(Key::Z, CTRL, false).unwrap(), [26]);
        assert_eq!(key_bytes(Key::OpenBracket, CTRL, false).unwrap(), [0x1b]);
    }

    #[test]
    fn plain_letters_come_from_text_events() {
        assert_eq!(key_bytes(Key::A, NONE, false), None);
    }

    #[test]
    fn digits_are_not_mistaken_for_letters() {
        assert_eq!(key_bytes(Key::Num1, CTRL, false), None);
    }

    #[test]
    fn editing_keys() {
        assert_eq!(key_bytes(Key::Backspace, NONE, false).unwrap(), [0x7f]);
        assert_eq!(key_bytes(Key::Delete, NONE, false).unwrap(), b"\x1b[3~");
        assert_eq!(key_bytes(Key::Tab, SHIFT, false).unwrap(), b"\x1b[Z");
        assert_eq!(key_bytes(Key::F5, NONE, false).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn bracketed_paste_strips_escapes() {
        let out = paste_bytes("a\x1b[201~rm -rf /\n", true);
        assert_eq!(out, b"\x1b[200~a[201~rm -rf /\n\x1b[201~");
    }

    #[test]
    fn plain_paste_uses_carriage_returns() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb");
    }

    #[test]
    fn sgr_mouse_is_one_based() {
        assert_eq!(mouse_report(0, 0, 0, true, true).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(mouse_report(0, 4, 2, false, true).unwrap(), b"\x1b[<0;5;3m");
    }

    #[test]
    fn legacy_mouse_rejects_large_coordinates() {
        assert!(mouse_report(0, 500, 0, true, false).is_none());
    }
}
