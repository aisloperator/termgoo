//! What the keyboard and mouse send to the program, as xterm encodes them.
//! The encoding is fixed here: nothing a program sends can change it, apart
//! from the standard cursor and keypad application modes.

use gtk::gdk::{Key, ModifierType as M};

#[derive(Clone, Copy, Default)]
pub struct KeyModes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub newline: bool,
}

fn modifier_param(mods: M) -> u8 {
    1 + mods.contains(M::SHIFT_MASK) as u8
        + 2 * mods.contains(M::ALT_MASK) as u8
        + 4 * mods.contains(M::CONTROL_MASK) as u8
}

fn tilde(code: u8, m: u8) -> Vec<u8> {
    if m == 1 { format!("\x1b[{code}~") } else { format!("\x1b[{code};{m}~") }.into_bytes()
}

/// Cursor-style keys: SS3 in application mode, CSI otherwise, CSI 1;m with modifiers.
fn letter(c: char, m: u8, ss3: bool) -> Vec<u8> {
    if m != 1 {
        format!("\x1b[1;{m}{c}")
    } else if ss3 {
        format!("\x1bO{c}")
    } else {
        format!("\x1b[{c}")
    }
    .into_bytes()
}

fn with_alt(mods: M, mut bytes: Vec<u8>) -> Vec<u8> {
    if mods.contains(M::ALT_MASK) {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn control_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' | '~' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

pub fn encode_key(key: Key, mods: M, modes: KeyModes) -> Option<Vec<u8>> {
    let m = modifier_param(mods);
    let ctrl = mods.contains(M::CONTROL_MASK);
    let keypad = |c: char| modes.app_keypad.then(|| format!("\x1bO{c}").into_bytes());
    let bytes = match key {
        Key::Up | Key::KP_Up => letter('A', m, modes.app_cursor),
        Key::Down | Key::KP_Down => letter('B', m, modes.app_cursor),
        Key::Right | Key::KP_Right => letter('C', m, modes.app_cursor),
        Key::Left | Key::KP_Left => letter('D', m, modes.app_cursor),
        Key::Home | Key::KP_Home => letter('H', m, modes.app_cursor),
        Key::End | Key::KP_End => letter('F', m, modes.app_cursor),
        Key::Insert | Key::KP_Insert => tilde(2, m),
        Key::Delete | Key::KP_Delete => tilde(3, m),
        Key::Page_Up | Key::KP_Page_Up => tilde(5, m),
        Key::Page_Down | Key::KP_Page_Down => tilde(6, m),
        Key::F1 => letter('P', m, true),
        Key::F2 => letter('Q', m, true),
        Key::F3 => letter('R', m, true),
        Key::F4 => letter('S', m, true),
        Key::F5 => tilde(15, m),
        Key::F6 => tilde(17, m),
        Key::F7 => tilde(18, m),
        Key::F8 => tilde(19, m),
        Key::F9 => tilde(20, m),
        Key::F10 => tilde(21, m),
        Key::F11 => tilde(23, m),
        Key::F12 => tilde(24, m),
        Key::Return | Key::KP_Enter if key == Key::KP_Enter && modes.app_keypad => b"\x1bOM".to_vec(),
        Key::Return | Key::KP_Enter => {
            with_alt(mods, if modes.newline { b"\r\n".to_vec() } else { b"\r".to_vec() })
        }
        Key::ISO_Left_Tab => b"\x1b[Z".to_vec(),
        Key::Tab if mods.contains(M::SHIFT_MASK) => b"\x1b[Z".to_vec(),
        Key::Tab => with_alt(mods, b"\t".to_vec()),
        Key::BackSpace => with_alt(mods, vec![if ctrl { 0x08 } else { 0x7f }]),
        Key::Escape => with_alt(mods, vec![0x1b]),
        Key::KP_0 | Key::KP_1 | Key::KP_2 | Key::KP_3 | Key::KP_4 | Key::KP_5 | Key::KP_6
        | Key::KP_7 | Key::KP_8 | Key::KP_9 if modes.app_keypad => {
            let digit = key.to_unicode()? as u8 - b'0';
            keypad((b'p' + digit) as char)?
        }
        Key::KP_Decimal if modes.app_keypad => keypad('n')?,
        Key::KP_Add if modes.app_keypad => keypad('k')?,
        Key::KP_Subtract if modes.app_keypad => keypad('m')?,
        Key::KP_Multiply if modes.app_keypad => keypad('j')?,
        Key::KP_Divide if modes.app_keypad => keypad('o')?,
        _ => {
            let c = key.to_unicode()?;
            if ctrl {
                with_alt(mods, vec![control_byte(c)?])
            } else {
                let mut buf = [0; 4];
                with_alt(mods, c.encode_utf8(&mut buf).as_bytes().to_vec())
            }
        }
    };
    Some(bytes)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// `button` is 0-2 for left, middle, right; 3 for "none" in motion; 64 and up for the wheel.
pub fn encode_mouse(action: MouseAction, button: u32, mods: M, col: usize, row: usize, sgr: bool) -> Vec<u8> {
    let mut code = button;
    code += 4 * mods.contains(M::SHIFT_MASK) as u32;
    code += 8 * mods.contains(M::ALT_MASK) as u32;
    code += 16 * mods.contains(M::CONTROL_MASK) as u32;
    if action == MouseAction::Motion {
        code += 32;
    }
    let (x, y) = (col + 1, row + 1);
    if sgr {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
        format!("\x1b[<{code};{x};{y}{end}").into_bytes()
    } else {
        // The old format has no way to say which button came up.
        let code = if action == MouseAction::Release { 3 + (code & !3) } else { code };
        // Each value is one byte, offset by 32, so coordinates top out at 223.
        let byte = |v: usize| (v.min(223) + 32) as u8;
        vec![0x1b, b'[', b'M', (code + 32).min(255) as u8, byte(x), byte(y)]
    }
}

/// What a paste becomes: control characters that could escape a bracketed
/// paste or act as commands are removed, and newlines are sent as Return.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let clean: String = normalized
        .chars()
        .filter(|&c| !c.is_control() || c == '\r' || c == '\t')
        .collect();
    if bracketed {
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        clean.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: M) -> Vec<u8> {
        encode_key(key, mods, KeyModes::default()).unwrap()
    }

    #[test]
    fn cursor_keys_and_modes() {
        assert_eq!(enc(Key::Up, M::empty()), b"\x1b[A");
        assert_eq!(enc(Key::Up, M::CONTROL_MASK), b"\x1b[1;5A");
        assert_eq!(enc(Key::Left, M::SHIFT_MASK | M::ALT_MASK), b"\x1b[1;4D");
        let app = KeyModes { app_cursor: true, ..KeyModes::default() };
        assert_eq!(encode_key(Key::Up, M::empty(), app).unwrap(), b"\x1bOA");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(Key::F1, M::empty()), b"\x1bOP");
        assert_eq!(enc(Key::F1, M::SHIFT_MASK), b"\x1b[1;2P");
        assert_eq!(enc(Key::F5, M::empty()), b"\x1b[15~");
        assert_eq!(enc(Key::F12, M::CONTROL_MASK), b"\x1b[24;5~");
    }

    #[test]
    fn control_and_alt() {
        assert_eq!(enc(Key::c, M::CONTROL_MASK), [3]);
        assert_eq!(enc(Key::bracketleft, M::CONTROL_MASK), [0x1b]);
        assert_eq!(enc(Key::space, M::CONTROL_MASK), [0]);
        assert_eq!(enc(Key::x, M::ALT_MASK), b"\x1bx");
        assert_eq!(enc(Key::BackSpace, M::empty()), [0x7f]);
        assert_eq!(enc(Key::BackSpace, M::CONTROL_MASK), [8]);
        assert_eq!(enc(Key::Tab, M::SHIFT_MASK), b"\x1b[Z");
    }

    #[test]
    fn keypad_application_mode() {
        let app = KeyModes { app_keypad: true, ..KeyModes::default() };
        assert_eq!(encode_key(Key::KP_5, M::empty(), app).unwrap(), b"\x1bOu");
        assert_eq!(encode_key(Key::KP_Enter, M::empty(), app).unwrap(), b"\x1bOM");
        assert_eq!(enc(Key::KP_5, M::empty()), b"5");
    }

    #[test]
    fn modifier_keys_alone_send_nothing() {
        assert_eq!(encode_key(Key::Shift_L, M::SHIFT_MASK, KeyModes::default()), None);
    }

    #[test]
    fn mouse_encodings() {
        assert_eq!(encode_mouse(MouseAction::Press, 0, M::empty(), 4, 9, true), b"\x1b[<0;5;10M");
        assert_eq!(encode_mouse(MouseAction::Release, 2, M::CONTROL_MASK, 0, 0, true), b"\x1b[<18;1;1m");
        assert_eq!(encode_mouse(MouseAction::Motion, 0, M::empty(), 0, 0, true), b"\x1b[<32;1;1M");
        assert_eq!(encode_mouse(MouseAction::Press, 0, M::empty(), 0, 0, false), [0x1b, b'[', b'M', 32, 33, 33]);
        assert_eq!(encode_mouse(MouseAction::Release, 2, M::empty(), 0, 0, false), [0x1b, b'[', b'M', 35, 33, 33]);
        assert_eq!(encode_mouse(MouseAction::Press, 64, M::empty(), 999, 0, false)[4], 255);
    }

    #[test]
    fn paste_cannot_break_out_of_brackets() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~rm\x03", true), b"\x1b[200~x[201~rm\x1b[201~");
        assert_eq!(paste_bytes("é\t", false), "é\t".as_bytes());
    }
}
