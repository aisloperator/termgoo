//! The escape sequence state machine, and what each sequence does.
//!
//! Sequences that would let the application remap keys are recognised and
//! dropped on purpose; see `dcs_start` and the `b'p'` and `b'm'` arms of
//! `csi_dispatch`.

use super::cell::{Color, UL_CURLY, UL_DASHED, UL_DOTTED, UL_DOUBLE, UL_NONE, UL_SINGLE, flag};
use super::sixel::{self, Sixel};
use super::{Charset, CursorShape, MouseMode, Terminal, VERSION_REPLY};
use std::fmt::Write;

const MAX_PARAMS: usize = 32;
const MAX_STRING: usize = 1 << 20;
const MAX_TITLE: usize = 4096;
const MAX_TITLE_STACK: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Ground,
    Escape,
    Csi,
    CsiIgnore,
    Osc,
    DcsParams,
    DcsString,
    /// Inside a device control string that is being thrown away.
    Ignore,
    /// SOS, PM or APC: ignored up to the string terminator.
    SosPmApc,
    Sixel,
}

pub(super) struct Parser {
    state: State,
    params: Vec<u16>,
    /// `subs[i]`: parameter i is attached to the previous one with a colon.
    subs: Vec<bool>,
    param_started: bool,
    intermediates: Vec<u8>,
    prefix: u8,
    string: Vec<u8>,
    sixel: Option<Sixel>,
    utf8_buf: u32,
    utf8_left: u8,
    utf8_min: u32,
}

impl Parser {
    pub(super) fn new() -> Parser {
        Parser {
            state: State::Ground,
            params: Vec::new(),
            subs: Vec::new(),
            param_started: false,
            intermediates: Vec::new(),
            prefix: 0,
            string: Vec::new(),
            sixel: None,
            utf8_buf: 0,
            utf8_left: 0,
            utf8_min: 0,
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.subs.clear();
        self.param_started = false;
        self.intermediates.clear();
        self.prefix = 0;
    }

    fn param(&self, i: usize, default: u16) -> u16 {
        match self.params.get(i) {
            Some(&0) | None => default,
            Some(&v) => v,
        }
    }

    fn digit(&mut self, b: u8) {
        if self.params.len() >= MAX_PARAMS {
            return;
        }
        if !self.param_started {
            self.params.push(0);
            self.subs.push(false);
            self.param_started = true;
        }
        let last = self.params.last_mut().unwrap();
        *last = last.saturating_mul(10).saturating_add((b - b'0') as u16);
    }

    fn separator(&mut self, b: u8) {
        if !self.param_started && self.params.len() < MAX_PARAMS {
            self.params.push(0);
            self.subs.push(false);
        }
        self.param_started = false;
        if self.params.len() < MAX_PARAMS {
            self.params.push(0);
            self.subs.push(b == b':');
            self.param_started = true;
        }
    }
}

impl Terminal {
    /// Process output from the application.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            // Fast path for the overwhelmingly common case of plain text.
            if self.parser.state == State::Ground && self.parser.utf8_left == 0 {
                while i < bytes.len() && (0x20..0x7f).contains(&bytes[i]) {
                    self.print(bytes[i] as char);
                    i += 1;
                }
                if i == bytes.len() {
                    break;
                }
            }
            let b = bytes[i];
            i += 1;
            self.feed_byte(b);
        }
    }

    fn feed_byte(&mut self, b: u8) {
        let p = &mut self.parser;
        if p.utf8_left > 0 {
            if b & 0xc0 == 0x80 {
                p.utf8_buf = p.utf8_buf << 6 | (b & 0x3f) as u32;
                p.utf8_left -= 1;
                if p.utf8_left == 0 {
                    let u = p.utf8_buf;
                    let c = if u < p.utf8_min { None } else { char::from_u32(u) };
                    self.advance(c.unwrap_or('\u{fffd}'));
                }
                return;
            }
            p.utf8_left = 0;
            self.advance('\u{fffd}');
        }
        let p = &mut self.parser;
        match b {
            0..=0x7f => self.advance(b as char),
            0xc2..=0xdf => (p.utf8_buf, p.utf8_left, p.utf8_min) = ((b & 0x1f) as u32, 1, 0x80),
            0xe0..=0xef => (p.utf8_buf, p.utf8_left, p.utf8_min) = ((b & 0x0f) as u32, 2, 0x800),
            0xf0..=0xf4 => (p.utf8_buf, p.utf8_left, p.utf8_min) = ((b & 0x07) as u32, 3, 0x10000),
            _ => self.advance('\u{fffd}'),
        }
    }

    fn string_ended(&mut self) {
        match self.parser.state {
            State::Osc => self.osc_dispatch(),
            State::DcsString => self.dcs_dispatch(),
            State::Sixel => {
                if let Some((w, h, rgba)) = self.parser.sixel.take().and_then(Sixel::finish) {
                    self.place_image(w, h, rgba);
                }
            }
            _ => {}
        }
        self.parser.string.clear();
    }

    fn advance(&mut self, c: char) {
        use State::*;
        let u = c as u32;
        let state = self.parser.state;
        let in_string = matches!(state, Osc | DcsParams | DcsString | Ignore | SosPmApc | Sixel);

        // Things that act the same in every state.
        match u {
            0x18 | 0x1a => {
                if state == Sixel {
                    self.parser.sixel = None;
                }
                self.parser.string.clear();
                self.parser.state = Ground;
                return;
            }
            0x1b => {
                self.string_ended();
                self.parser.clear();
                self.parser.state = Escape;
                return;
            }
            0x9c => {
                self.string_ended();
                self.parser.state = Ground;
                return;
            }
            0x07 if state == Osc => {
                self.string_ended();
                self.parser.state = Ground;
                return;
            }
            0x80..=0x9f if !in_string => {
                // A C1 control is shorthand for ESC + (c - 0x40).
                self.parser.clear();
                self.escape_dispatch((u - 0x40) as u8);
                return;
            }
            _ => {}
        }

        if u < 0x20 && !in_string {
            self.execute(u as u8);
            return;
        }

        match state {
            Ground => {
                if u != 0x7f {
                    self.print(c);
                }
            }
            Escape => {
                if (0x20..0x30).contains(&u) {
                    if self.parser.intermediates.len() < 2 {
                        self.parser.intermediates.push(u as u8);
                    }
                } else if u != 0x7f {
                    self.parser.state = Ground;
                    self.escape_dispatch(u.min(0xff) as u8);
                }
            }
            Csi => {
                let b = u.min(0xff) as u8;
                match b {
                    b'0'..=b'9' => self.parser.digit(b),
                    b';' | b':' => self.parser.separator(b),
                    b'<'..=b'?' => {
                        if self.parser.params.is_empty() && self.parser.prefix == 0 {
                            self.parser.prefix = b;
                        } else {
                            self.parser.state = CsiIgnore;
                        }
                    }
                    0x20..=0x2f => {
                        if self.parser.intermediates.len() < 2 {
                            self.parser.intermediates.push(b);
                        } else {
                            self.parser.state = CsiIgnore;
                        }
                    }
                    0x40..=0x7e => {
                        self.parser.state = Ground;
                        self.csi_dispatch(b);
                    }
                    _ => {}
                }
            }
            CsiIgnore => {
                if (0x40..=0x7e).contains(&u) {
                    self.parser.state = Ground;
                }
            }
            Osc => {
                if self.parser.string.len() < MAX_STRING {
                    let mut buf = [0; 4];
                    self.parser.string.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
            DcsParams => {
                let b = u.min(0xff) as u8;
                match b {
                    b'0'..=b'9' => self.parser.digit(b),
                    b';' | b':' => self.parser.separator(b),
                    b'<'..=b'?' if self.parser.params.is_empty() => self.parser.prefix = b,
                    0x20..=0x2f => {
                        if self.parser.intermediates.len() < 2 {
                            self.parser.intermediates.push(b);
                        }
                    }
                    0x40..=0x7e => self.dcs_start(b),
                    _ => {}
                }
            }
            DcsString => {
                if self.parser.string.len() < 4096 && u < 0x80 {
                    self.parser.string.push(u as u8);
                }
            }
            Sixel => {
                if let Some(s) = self.parser.sixel.as_mut().filter(|_| u < 0x80) {
                    s.feed(u as u8);
                }
            }
            Ignore | SosPmApc => {}
        }
    }

    fn execute(&mut self, b: u8) {
        match b {
            0x07 => self.bell = true,
            0x08 => self.move_left(1),
            0x09 => self.tab(1),
            0x0a..=0x0c => {
                self.index();
                if self.newline_mode {
                    self.cur.col = 0;
                }
                self.cur.wrap_pending = false;
            }
            0x0d => {
                self.cur.col = 0;
                self.cur.wrap_pending = false;
            }
            0x0e => self.cur.gl = 1,
            0x0f => self.cur.gl = 0,
            _ => {}
        }
    }

    fn escape_dispatch(&mut self, b: u8) {
        let first = self.parser.intermediates.first().copied();
        self.parser.state = State::Ground;
        match (first, b) {
            (None, b'[') => {
                self.parser.clear();
                self.parser.state = State::Csi;
            }
            (None, b']') => {
                self.parser.string.clear();
                self.parser.state = State::Osc;
            }
            (None, b'P') => {
                self.parser.clear();
                self.parser.string.clear();
                self.parser.state = State::DcsParams;
            }
            (None, b'X' | b'^' | b'_') => self.parser.state = State::SosPmApc,
            (None, b'\\') => {}
            (None, b'7') => self.save_cursor(),
            (None, b'8') => self.restore_cursor(),
            (None, b'D') => self.index(),
            (None, b'E') => {
                self.index();
                self.cur.col = 0;
                self.cur.wrap_pending = false;
            }
            (None, b'H') => {
                let col = self.cur.col;
                self.tabs[col] = true;
            }
            (None, b'M') => self.reverse_index(),
            (None, b'N') => self.cur.single_shift = Some(2),
            (None, b'O') => self.cur.single_shift = Some(3),
            (None, b'Z') => self.primary_da(),
            (None, b'c') => self.reset(),
            (None, b'=') => self.app_keypad = true,
            (None, b'>') => self.app_keypad = false,
            (None, b'n') => self.cur.gl = 2,
            (None, b'o') => self.cur.gl = 3,
            (Some(b'#'), b'8') => self.alignment_test(),
            (Some(i @ (b'(' | b')' | b'*' | b'+')), f) => {
                let set = match f {
                    b'0' => Charset::DecGraphics,
                    b'A' => Charset::Uk,
                    _ => Charset::Ascii,
                };
                self.cur.charsets[(i - b'(') as usize] = set;
            }
            _ => {}
        }
        self.parser.intermediates.clear();
    }

    fn alignment_test(&mut self) {
        let mut cell = super::Cell::blank(Color::Default);
        cell.c = 'E';
        for row in self.screen() {
            row.cells.fill(cell.clone());
            row.wrapped = false;
        }
        self.top = 0;
        self.bottom = self.rows - 1;
        self.cur.row = 0;
        self.cur.col = 0;
        self.cur.wrap_pending = false;
    }

    fn primary_da(&mut self) {
        // VT220 class, with Sixel (4), selective erase (6) and ANSI colour (22).
        self.replies.extend_from_slice(b"\x1b[?62;4;6;22c");
    }

    fn csi_dispatch(&mut self, b: u8) {
        let prefix = self.parser.prefix;
        let inter = self.parser.intermediates.first().copied().unwrap_or(0);
        let n = self.parser.param(0, 1) as usize;
        let p0 = self.parser.params.first().copied().unwrap_or(0);
        match (prefix, inter, b) {
            (0, 0, b'@') => self.insert_cells(n),
            (0, 0, b'A') => self.move_up(n),
            (0, 0, b'B' | b'e') => self.move_down(n),
            (0, 0, b'C' | b'a') => self.move_right(n),
            (0, 0, b'D') => self.move_left(n),
            (0, 0, b'E') => {
                self.move_down(n);
                self.cur.col = 0;
            }
            (0, 0, b'F') => {
                self.move_up(n);
                self.cur.col = 0;
            }
            (0, 0, b'G' | b'`') => {
                self.cur.col = (n - 1).min(self.cols - 1);
                self.cur.wrap_pending = false;
            }
            (0, 0, b'H' | b'f') => {
                let col = self.parser.param(1, 1) as usize;
                self.goto(n - 1, col - 1);
            }
            (0, 0, b'I') => self.tab(n),
            (0, 0, b'J') => self.erase_in_display(p0, false),
            (b'?', 0, b'J') => self.erase_in_display(p0, true),
            (0, 0, b'K') => self.erase_in_line(p0, false),
            (b'?', 0, b'K') => self.erase_in_line(p0, true),
            (0, 0, b'L') => self.insert_lines(n),
            (0, 0, b'M') => self.delete_lines(n),
            (0, 0, b'P') => self.delete_cells(n),
            (0, 0, b'S') => self.scroll_up(n),
            (0, 0, b'T') if self.parser.params.len() <= 1 => self.scroll_down(n),
            (0, 0, b'X') => {
                let (row, col) = (self.cur.row, self.cur.col);
                self.erase_cells(row, col, col.saturating_add(n), false);
                self.cur.wrap_pending = false;
            }
            (0, 0, b'Z') => self.back_tab(n),
            (0, 0, b'b') => {
                if let Some(c) = self.last_printed {
                    (0..n.min(self.cols * self.rows)).for_each(|_| self.print(c));
                }
            }
            (0, 0, b'c') if p0 == 0 => self.primary_da(),
            (b'>', 0, b'c') if p0 == 0 => self.replies.extend_from_slice(b"\x1b[>1;100;0c"),
            (0, 0, b'd') => {
                let col = self.cur.col;
                self.goto(n - 1, col);
            }
            (0, 0, b'g') => match p0 {
                0 => {
                    let col = self.cur.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            (0, 0, b'h') => self.set_ansi_modes(true),
            (0, 0, b'l') => self.set_ansi_modes(false),
            (b'?', 0, b'h') => self.set_private_modes(true),
            (b'?', 0, b'l') => self.set_private_modes(false),
            (0, 0, b'm') => self.sgr(),
            // XTMODKEYS (`CSI > ... m`), like every other way of changing what
            // keys send, is refused: it falls through to the ignore arm below.
            (0, 0, b'n') => self.device_status(p0),
            (b'?', 0, b'n') if p0 == 6 => {
                let (r, c) = (self.cur.row + 1, self.cur.col + 1);
                self.reply(format_args!("\x1b[?{r};{c}R"));
            }
            (0, b'!', b'p') => self.soft_reset(),
            (0, b'$', b'p') => self.report_ansi_mode(p0),
            (b'?', b'$', b'p') => self.report_private_mode(p0),
            // Plain `CSI ... p` is the ANSI.SYS keyboard reassignment sequence: refused.
            (b'?', 0, b'S') => self.graphics_attributes(),
            (0, b' ', b'q') => {
                (self.cursor_shape, self.cursor_blink) = match p0 {
                    0 | 1 => (CursorShape::Block, true),
                    2 => (CursorShape::Block, false),
                    3 => (CursorShape::Underline, true),
                    4 => (CursorShape::Underline, false),
                    5 => (CursorShape::Bar, true),
                    6 => (CursorShape::Bar, false),
                    _ => (self.cursor_shape, self.cursor_blink),
                };
            }
            (0, b'"', b'q') => {
                if p0 == 1 {
                    self.cur.pen.flags |= flag::PROTECTED;
                } else {
                    self.cur.pen.flags &= !flag::PROTECTED;
                }
            }
            (b'>', 0, b'q') => {
                self.reply(format_args!("\x1bP>|{VERSION_REPLY}\x1b\\"));
            }
            (0, 0, b'r') => {
                let top = n - 1;
                let bottom = (self.parser.param(1, self.rows as u16) as usize).min(self.rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            (0, 0, b's') => self.save_cursor(),
            (0, 0, b't') => self.window_op(p0),
            (0, 0, b'u') => self.restore_cursor(),
            _ => {}
        }
    }

    fn reply(&mut self, args: std::fmt::Arguments) {
        let mut s = String::new();
        let _ = s.write_fmt(args);
        self.replies.extend_from_slice(s.as_bytes());
    }

    fn device_status(&mut self, p: u16) {
        match p {
            5 => self.replies.extend_from_slice(b"\x1b[0n"),
            6 => {
                let row = if self.cur.origin { self.cur.row.saturating_sub(self.top) } else { self.cur.row };
                let (r, c) = (row + 1, self.cur.col + 1);
                self.reply(format_args!("\x1b[{r};{c}R"));
            }
            _ => {}
        }
    }

    fn window_op(&mut self, op: u16) {
        // Only reports and the title stack. Applications may not move or resize the window.
        let (rows, cols) = (self.rows, self.cols);
        let (cw, ch) = (self.cell_w as usize, self.cell_h as usize);
        match op {
            14 => self.reply(format_args!("\x1b[4;{};{}t", rows * ch, cols * cw)),
            16 => self.reply(format_args!("\x1b[6;{ch};{cw}t")),
            18 => self.reply(format_args!("\x1b[8;{rows};{cols}t")),
            19 => self.reply(format_args!("\x1b[9;{rows};{cols}t")),
            22 => {
                if self.title_stack.len() < MAX_TITLE_STACK {
                    self.title_stack.push(self.title.clone());
                }
            }
            23 => {
                if let Some(t) = self.title_stack.pop() {
                    self.set_title(t);
                }
            }
            _ => {}
        }
    }

    fn set_title(&mut self, mut t: String) {
        if t.len() > MAX_TITLE {
            let mut end = MAX_TITLE;
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            t.truncate(end);
        }
        t.retain(|c| !c.is_control());
        self.title = t.clone();
        self.new_title = Some(t);
    }

    fn graphics_attributes(&mut self) {
        // XTSMGRAPHICS: only "read" (1) of colour registers (1) and geometry (2).
        let item = self.parser.param(0, 0);
        let action = self.parser.param(1, 0);
        let max = sixel::MAX_DIMENSION;
        match (item, action) {
            (1, 1 | 4) => self.reply(format_args!("\x1b[?1;0;{}S", sixel::REGISTERS)),
            (2, 1 | 4) => self.reply(format_args!("\x1b[?2;0;{max};{max}S")),
            _ => self.reply(format_args!("\x1b[?{item};1;0S")),
        }
    }

    fn set_ansi_modes(&mut self, on: bool) {
        for i in 0..self.parser.params.len() {
            match self.parser.params[i] {
                4 => self.insert_mode = on,
                20 => self.newline_mode = on,
                _ => {}
            }
        }
    }

    fn ansi_mode(&self, p: u16) -> Option<bool> {
        match p {
            4 => Some(self.insert_mode),
            20 => Some(self.newline_mode),
            _ => None,
        }
    }

    fn report_ansi_mode(&mut self, p: u16) {
        let s = self.ansi_mode(p).map_or(0, |on| if on { 1 } else { 2 });
        self.reply(format_args!("\x1b[{p};{s}$y"));
    }

    fn private_mode(&self, p: u16) -> Option<bool> {
        Some(match p {
            1 => self.app_cursor,
            5 => self.reverse_video,
            6 => self.cur.origin,
            7 => self.cur.autowrap,
            9 => self.mouse_mode == MouseMode::X10,
            12 => self.cursor_blink,
            25 => self.cursor_visible,
            47 | 1047 | 1049 => self.using_alt,
            66 => self.app_keypad,
            80 => !self.sixel_scrolling,
            1000 => self.mouse_mode == MouseMode::Normal,
            1002 => self.mouse_mode == MouseMode::Drag,
            1003 => self.mouse_mode == MouseMode::Any,
            1004 => self.focus_reporting,
            1006 => self.mouse_sgr,
            1007 => self.alternate_scroll,
            2004 => self.bracketed_paste,
            8452 => self.sixel_cursor_right,
            _ => return None,
        })
    }

    fn report_private_mode(&mut self, p: u16) {
        let s = self.private_mode(p).map_or(0, |on| if on { 1 } else { 2 });
        self.reply(format_args!("\x1b[?{p};{s}$y"));
    }

    fn set_mouse(&mut self, on: bool, mode: MouseMode) {
        if on {
            self.mouse_mode = mode;
        } else if self.mouse_mode == mode {
            self.mouse_mode = MouseMode::Off;
        }
    }

    fn set_private_modes(&mut self, on: bool) {
        for i in 0..self.parser.params.len() {
            match self.parser.params[i] {
                1 => self.app_cursor = on,
                5 => self.reverse_video = on,
                6 => {
                    self.cur.origin = on;
                    self.goto(0, 0);
                }
                7 => {
                    self.cur.autowrap = on;
                    self.cur.wrap_pending = false;
                }
                9 => self.set_mouse(on, MouseMode::X10),
                12 => self.cursor_blink = on,
                25 => self.cursor_visible = on,
                47 => self.set_alt_screen(on),
                66 => self.app_keypad = on,
                80 => self.sixel_scrolling = !on,
                1000 => self.set_mouse(on, MouseMode::Normal),
                1002 => self.set_mouse(on, MouseMode::Drag),
                1003 => self.set_mouse(on, MouseMode::Any),
                1004 => self.focus_reporting = on,
                1006 => self.mouse_sgr = on,
                1007 => self.alternate_scroll = on,
                1047 => {
                    if !on && self.using_alt {
                        self.clear_alt_screen();
                    }
                    self.set_alt_screen(on);
                }
                1048 => {
                    if on { self.save_cursor() } else { self.restore_cursor() }
                }
                1049 => {
                    if on {
                        if !self.using_alt {
                            self.save_cursor();
                            self.set_alt_screen(true);
                            self.clear_alt_screen();
                        }
                    } else if self.using_alt {
                        self.set_alt_screen(false);
                        self.restore_cursor();
                    }
                }
                2004 => self.bracketed_paste = on,
                8452 => self.sixel_cursor_right = on,
                _ => {}
            }
        }
    }

    fn sgr(&mut self) {
        let params = std::mem::take(&mut self.parser.params);
        let subs = std::mem::take(&mut self.parser.subs);
        let set_ul = |pen: &mut super::Pen, style: u16| {
            pen.flags = pen.flags & !flag::UL_MASK | style << flag::UL_SHIFT;
        };
        let pen = &mut self.cur.pen;
        if params.is_empty() {
            *pen = super::Pen { flags: pen.flags & flag::PROTECTED, ..super::Pen::new() };
        }
        let mut i = 0;
        while i < params.len() {
            // `j` is the first parameter after this one's colon-attached group.
            let mut j = i + 1;
            while j < params.len() && subs[j] {
                j += 1;
            }
            let group = &params[i + 1..j];
            match params[i] {
                0 => *pen = super::Pen { flags: pen.flags & flag::PROTECTED, ..super::Pen::new() },
                1 => pen.flags |= flag::BOLD,
                2 => pen.flags |= flag::DIM,
                3 => pen.flags |= flag::ITALIC,
                4 => {
                    let style = match group.first() {
                        None | Some(1) => UL_SINGLE,
                        Some(2) => UL_DOUBLE,
                        Some(3) => UL_CURLY,
                        Some(4) => UL_DOTTED,
                        Some(5) => UL_DASHED,
                        _ => UL_NONE,
                    };
                    set_ul(pen, style);
                }
                5 | 6 => pen.flags |= flag::BLINK,
                7 => pen.flags |= flag::INVERSE,
                8 => pen.flags |= flag::HIDDEN,
                9 => pen.flags |= flag::STRIKE,
                21 => set_ul(pen, UL_DOUBLE),
                22 => pen.flags &= !(flag::BOLD | flag::DIM),
                23 => pen.flags &= !flag::ITALIC,
                24 => set_ul(pen, UL_NONE),
                25 => pen.flags &= !flag::BLINK,
                27 => pen.flags &= !flag::INVERSE,
                28 => pen.flags &= !flag::HIDDEN,
                29 => pen.flags &= !flag::STRIKE,
                30..=37 => pen.fg = Color::Indexed((params[i] - 30) as u8),
                39 => pen.fg = Color::Default,
                40..=47 => pen.bg = Color::Indexed((params[i] - 40) as u8),
                49 => pen.bg = Color::Default,
                53 => pen.flags |= flag::OVERLINE,
                55 => pen.flags &= !flag::OVERLINE,
                59 => pen.underline_color = None,
                90..=97 => pen.fg = Color::Indexed((params[i] - 90 + 8) as u8),
                100..=107 => pen.bg = Color::Indexed((params[i] - 100 + 8) as u8),
                38 | 48 | 58 => {
                    let (color, next) = if group.is_empty() {
                        semicolon_color(&params[i + 1..], i + 1)
                    } else {
                        (colon_color(group), j)
                    };
                    j = next.max(j);
                    if let Some(color) = color {
                        match params[i] {
                            38 => pen.fg = color,
                            48 => pen.bg = color,
                            _ => pen.underline_color = Some(color),
                        }
                    }
                }
                _ => {}
            }
            i = j;
        }
    }

    // ---- Device control strings -------------------------------------------------

    fn dcs_start(&mut self, b: u8) {
        let inter = self.parser.intermediates.first().copied().unwrap_or(0);
        self.parser.state = match (self.parser.prefix, inter, b) {
            (0, 0, b'q') => {
                self.parser.sixel = Some(Sixel::new());
                State::Sixel
            }
            (0, b'$', b'q') | (0, b'+', b'q') => State::DcsString,
            // DECUDK (`DCS ... |`) defines what function keys send, and xterm's
            // `DCS + p` sets its resources. Both are refused.
            _ => State::Ignore,
        };
        self.parser.string.clear();
    }

    fn dcs_dispatch(&mut self) {
        let inter = self.parser.intermediates.first().copied().unwrap_or(0);
        if inter == b'+' {
            // XTGETTCAP: the terminfo entry is the place to look; answer "unknown".
            self.replies.extend_from_slice(b"\x1bP0+r\x1b\\");
            return;
        }
        // DECRQSS: report a setting.
        let request = std::mem::take(&mut self.parser.string);
        let body = match request.as_slice() {
            b"m" => Some(self.pen_sgr() + "m"),
            b"r" => Some(format!("{};{}r", self.top + 1, self.bottom + 1)),
            b"\"p" => Some("62;1\"p".to_string()),
            b"\"q" => Some(format!("{}\"q", (self.cur.pen.flags & flag::PROTECTED != 0) as u8)),
            b" q" => {
                let base = match self.cursor_shape {
                    CursorShape::Block => 1,
                    CursorShape::Underline => 3,
                    CursorShape::Bar => 5,
                };
                Some(format!("{} q", base + !self.cursor_blink as u8))
            }
            _ => None,
        };
        match body {
            Some(body) => self.reply(format_args!("\x1bP1$r{body}\x1b\\")),
            None => self.replies.extend_from_slice(b"\x1bP0$r\x1b\\"),
        }
    }

    fn pen_sgr(&self) -> String {
        let pen = &self.cur.pen;
        let mut s = String::from("0");
        for (bit, code) in [
            (flag::BOLD, 1), (flag::DIM, 2), (flag::ITALIC, 3), (flag::BLINK, 5),
            (flag::INVERSE, 7), (flag::HIDDEN, 8), (flag::STRIKE, 9), (flag::OVERLINE, 53),
        ] {
            if pen.flags & bit != 0 {
                let _ = write!(s, ";{code}");
            }
        }
        if pen.flags & flag::UL_MASK != 0 {
            s.push_str(";4");
        }
        for (color, base) in [(pen.fg, 38), (pen.bg, 48)] {
            match color {
                Color::Default => {}
                Color::Indexed(i) => {
                    let _ = write!(s, ";{base};5;{i}");
                }
                Color::Rgb(r, g, b) => {
                    let _ = write!(s, ";{base};2;{r};{g};{b}");
                }
            }
        }
        s
    }

    // ---- Operating system commands ----------------------------------------------

    fn osc_dispatch(&mut self) {
        let data = std::mem::take(&mut self.parser.string);
        let text = String::from_utf8_lossy(&data);
        let (cmd, rest) = text.split_once(';').unwrap_or((&text, ""));
        let terminator = "\x1b\\";
        match cmd {
            "0" | "2" => self.set_title(rest.to_string()),
            "4" => {
                let mut parts = rest.split(';');
                while let (Some(idx), Some(spec)) = (parts.next(), parts.next()) {
                    let Ok(idx) = idx.parse::<u8>() else { continue };
                    if spec == "?" {
                        let rgb = self.palette_color(idx);
                        let s = color_spec(rgb);
                        self.reply(format_args!("\x1b]4;{idx};{s}{terminator}"));
                    } else if let Some(rgb) = parse_color(spec) {
                        self.palette_overrides[idx as usize] = Some(rgb);
                    }
                }
            }
            "10" | "11" | "12" => {
                // Each further `;` item applies to the next colour in the series.
                let base: u8 = cmd.parse().unwrap();
                for (i, spec) in rest.split(';').enumerate().take(3) {
                    let which = base as usize + i;
                    if which > 12 {
                        break;
                    }
                    if spec == "?" {
                        let rgb = match which {
                            10 => self.fg_override.unwrap_or(self.theme_colors.fg),
                            11 => self.bg_override.unwrap_or(self.theme_colors.bg),
                            _ => self.cursor_override.unwrap_or(self.theme_colors.cursor),
                        };
                        let s = color_spec(rgb);
                        self.reply(format_args!("\x1b]{which};{s}{terminator}"));
                    } else if let Some(rgb) = parse_color(spec) {
                        match which {
                            10 => self.fg_override = Some(rgb),
                            11 => self.bg_override = Some(rgb),
                            _ => self.cursor_override = Some(rgb),
                        }
                    }
                }
            }
            "104" => {
                if rest.is_empty() {
                    self.palette_overrides.fill(None);
                }
                for idx in rest.split(';').filter_map(|s| s.parse::<u8>().ok()) {
                    self.palette_overrides[idx as usize] = None;
                }
            }
            "110" => self.fg_override = None,
            "111" => self.bg_override = None,
            "112" => self.cursor_override = None,
            // OSC 52 (clipboard) is deliberately ignored: output from a program
            // must not be able to read or replace what the user copied.
            _ => {}
        }
    }

    pub fn palette_color(&self, idx: u8) -> (u8, u8, u8) {
        self.palette_overrides[idx as usize].unwrap_or(self.theme_colors.palette[idx as usize])
    }
}

fn semicolon_color(rest: &[u16], at: usize) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) => (rest.get(1).map(|&v| Color::Indexed(v.min(255) as u8)), at + 2),
        Some(2) if rest.len() >= 4 => {
            let c = |i: usize| rest[i].min(255) as u8;
            (Some(Color::Rgb(c(1), c(2), c(3))), at + 4)
        }
        _ => (None, at + rest.len()),
    }
}

fn colon_color(group: &[u16]) -> Option<Color> {
    match group {
        [5, v, ..] => Some(Color::Indexed(*v.min(&255) as u8)),
        // `2:r:g:b` and `2:colorspace:r:g:b` are both in use.
        [2, r, g, b] | [2, _, r, g, b, ..] => {
            Some(Color::Rgb(*r.min(&255) as u8, *g.min(&255) as u8, *b.min(&255) as u8))
        }
        _ => None,
    }
}

fn color_spec((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// Parse `rgb:r/g/b` (1 to 4 hex digits each) or `#rrggbb`.
pub fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 && hex.is_ascii() {
            let c = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            return Some((c(0)?, c(2)?, c(4)?));
        }
        return None;
    }
    let mut parts = s.strip_prefix("rgb:")?.split('/');
    let mut c = || -> Option<u8> {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        Some((v * 255 / ((1u32 << (p.len() * 4)) - 1)) as u8)
    };
    Some((c()?, c()?, c()?))
}
