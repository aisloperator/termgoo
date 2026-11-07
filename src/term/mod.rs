//! Terminal state: the screens, the scrollback, and the operations on them.
//! `parser.rs` turns the byte stream into calls to these. Nothing here touches
//! the display, so all of it can be tested without a window.

pub mod cell;
mod parser;
pub use parser::parse_color;
mod reflow;
pub mod sixel;
#[cfg(test)]
mod tests;

use cell::{Cell, Color, Extra, Image, Row, Tile, char_width, flag};
use gtk::glib;
use parser::Parser;
use std::collections::VecDeque;
use std::rc::Rc;

pub const VERSION_REPLY: &str = concat!("termgoo ", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseMode {
    Off,
    /// Button press only (mode 9).
    X10,
    /// Press and release (mode 1000).
    Normal,
    /// Also motion while a button is held (mode 1002).
    Drag,
    /// All motion (mode 1003).
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Char,
    Word,
    Line,
}

/// A position in the whole history: an absolute line number and a column.
/// Line numbers do not change as text scrolls into the scrollback.
pub type Pos = (u64, usize);

#[derive(Clone, Copy)]
pub struct Selection {
    pub mode: SelectMode,
    pub anchor: Pos,
    pub start: Pos,
    /// Inclusive.
    pub end: Pos,
}

impl Selection {
    pub fn contains(&self, p: Pos) -> bool {
        self.start <= p && p <= self.end
    }
}

#[derive(Clone, Copy)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: u16,
    underline_color: Option<Color>,
}

impl Pen {
    fn new() -> Pen {
        Pen { fg: Color::Default, bg: Color::Default, flags: 0, underline_color: None }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Charset {
    Ascii,
    DecGraphics,
    Uk,
}

/// What DECSC saves.
#[derive(Clone, Copy)]
struct Cursor {
    row: usize,
    col: usize,
    pen: Pen,
    origin: bool,
    autowrap: bool,
    wrap_pending: bool,
    charsets: [Charset; 4],
    gl: usize,
    single_shift: Option<usize>,
}

impl Cursor {
    fn new() -> Cursor {
        Cursor {
            row: 0,
            col: 0,
            pen: Pen::new(),
            origin: false,
            autowrap: true,
            wrap_pending: false,
            charsets: [Charset::Ascii; 4],
            gl: 0,
            single_shift: None,
        }
    }
}

pub struct Terminal {
    pub cols: usize,
    pub rows: usize,
    /// Pixel size of a cell, for size reports and Sixel placement.
    pub cell_w: u32,
    pub cell_h: u32,

    primary: Vec<Row>,
    alternate: Vec<Row>,
    pub using_alt: bool,
    scrollback: VecDeque<Row>,
    max_scrollback: usize,
    /// Count of rows that ever entered the scrollback, the base of absolute line numbers.
    pushed: u64,
    /// How many rows the view is scrolled back from the live screen.
    pub view_offset: usize,

    cur: Cursor,
    saved: [Cursor; 2],
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    last_printed: Option<char>,

    pub cursor_visible: bool,
    pub cursor_blink: bool,
    pub cursor_shape: CursorShape,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub reverse_video: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub alternate_scroll: bool,
    pub mouse_mode: MouseMode,
    pub mouse_sgr: bool,
    pub newline_mode: bool,
    insert_mode: bool,
    sixel_scrolling: bool,
    sixel_cursor_right: bool,

    /// OSC 4 / 10 / 11 / 12 overrides on top of the active theme.
    pub palette_overrides: Box<[Option<(u8, u8, u8)>; 256]>,
    pub fg_override: Option<(u8, u8, u8)>,
    pub bg_override: Option<(u8, u8, u8)>,
    pub cursor_override: Option<(u8, u8, u8)>,
    /// Theme colours, for answering OSC colour queries.
    pub theme_colors: ThemeColors,

    pub selection: Option<Selection>,

    /// Bytes to send back to the application.
    pub replies: Vec<u8>,
    /// Set when the application changed the window title.
    pub new_title: Option<String>,
    pub bell: bool,
    title: String,
    title_stack: Vec<String>,

    parser: Parser,
}

/// The colours of the active theme, as needed by colour queries.
#[derive(Clone)]
pub struct ThemeColors {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub palette: [(u8, u8, u8); 256],
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize, theme_colors: ThemeColors) -> Terminal {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let blank = Cell::blank(Color::Default);
        Terminal {
            cols,
            rows,
            cell_w: 8,
            cell_h: 16,
            primary: vec![Row::new(cols, &blank); rows],
            alternate: vec![Row::new(cols, &blank); rows],
            using_alt: false,
            scrollback: VecDeque::new(),
            max_scrollback,
            pushed: 0,
            view_offset: 0,
            cur: Cursor::new(),
            saved: [Cursor::new(); 2],
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            last_printed: None,
            cursor_visible: true,
            cursor_blink: false,
            cursor_shape: CursorShape::Block,
            app_cursor: false,
            app_keypad: false,
            reverse_video: false,
            bracketed_paste: false,
            focus_reporting: false,
            alternate_scroll: true,
            mouse_mode: MouseMode::Off,
            mouse_sgr: false,
            newline_mode: false,
            insert_mode: false,
            sixel_scrolling: true,
            sixel_cursor_right: false,
            palette_overrides: Box::new([None; 256]),
            fg_override: None,
            bg_override: None,
            cursor_override: None,
            theme_colors,
            selection: None,
            replies: Vec::new(),
            new_title: None,
            bell: false,
            title: String::new(),
            title_stack: Vec::new(),
            parser: Parser::new(),
        }
    }

    fn screen(&mut self) -> &mut Vec<Row> {
        if self.using_alt { &mut self.alternate } else { &mut self.primary }
    }

    fn screen_ref(&self) -> &Vec<Row> {
        if self.using_alt { &self.alternate } else { &self.primary }
    }

    fn blank(&self) -> Cell {
        Cell::blank(self.cur.pen.bg)
    }

    // ---- Queries for the display ------------------------------------------------

    pub fn scrollback_len(&self) -> usize {
        if self.using_alt { 0 } else { self.scrollback.len() }
    }

    /// The row to show at screen line `y` given the current view offset.
    pub fn display_row(&self, y: usize) -> &Row {
        let sb = self.scrollback_len();
        let offset = self.view_offset.min(sb);
        if y < offset {
            &self.scrollback[sb - offset + y]
        } else {
            &self.screen_ref()[y - offset]
        }
    }

    /// Absolute line number of screen line `y` given the current view offset.
    pub fn display_line(&self, y: usize) -> u64 {
        self.pushed + y as u64 - self.view_offset.min(self.scrollback_len()) as u64
    }

    fn line(&self, abs: u64) -> Option<&Row> {
        if abs >= self.pushed {
            self.screen_ref().get((abs - self.pushed) as usize)
        } else {
            let back = (self.pushed - abs) as usize;
            let len = self.scrollback_len();
            if back <= len { self.scrollback.get(len - back) } else { None }
        }
    }

    fn first_line(&self) -> u64 {
        self.pushed - self.scrollback_len() as u64
    }

    fn last_line(&self) -> u64 {
        self.pushed + self.rows as u64 - 1
    }

    /// Where the cursor is drawn, if it is on the part of the screen in view.
    pub fn cursor_pos(&self) -> Option<(usize, usize)> {
        if !self.cursor_visible {
            return None;
        }
        let y = self.cur.row + self.view_offset.min(self.scrollback_len());
        (y < self.rows).then_some((y, self.cur.col))
    }

    pub fn scroll_view(&mut self, delta: isize) {
        let max = self.scrollback_len() as isize;
        self.view_offset = (self.view_offset as isize + delta).clamp(0, max) as usize;
    }

    pub fn set_max_scrollback(&mut self, n: usize) {
        self.max_scrollback = n;
        while self.scrollback.len() > n {
            self.scrollback.pop_front();
        }
        self.view_offset = self.view_offset.min(self.scrollback.len());
    }

    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
        self.view_offset = 0;
        self.selection = None;
    }

    // ---- Selection --------------------------------------------------------------

    pub fn select_start(&mut self, pos: Pos, mode: SelectMode) {
        self.selection = Some(Selection { mode, anchor: pos, start: pos, end: pos });
        self.select_update(pos);
    }

    pub fn select_update(&mut self, pos: Pos) {
        let Some(sel) = self.selection else { return };
        let (a, b) = if sel.anchor <= pos { (sel.anchor, pos) } else { (pos, sel.anchor) };
        let (start, end) = match sel.mode {
            SelectMode::Char => (a, b),
            SelectMode::Word => (self.word_bounds(a).0, self.word_bounds(b).1),
            SelectMode::Line => ((a.0, 0), (b.0, self.cols.saturating_sub(1))),
        };
        self.selection = Some(Selection { start, end, ..sel });
    }

    pub fn select_all(&mut self) {
        let a = (self.first_line(), 0);
        self.selection = Some(Selection {
            mode: SelectMode::Char,
            anchor: a,
            start: a,
            end: (self.last_line(), self.cols.saturating_sub(1)),
        });
    }

    fn cell_at(&self, p: Pos) -> Option<&Cell> {
        self.line(p.0)?.cells.get(p.1)
    }

    fn word_bounds(&self, p: Pos) -> (Pos, Pos) {
        fn class(c: &Cell) -> u8 {
            if c.is_wide_cont() {
                1
            } else if c.c == ' ' {
                0
            } else if c.c.is_alphanumeric() || "_-./~+%@:#=?&".contains(c.c) {
                1
            } else {
                2
            }
        }
        let class_at = |p: Pos| self.cell_at(p).map_or(0, class);
        let want = class_at(p);
        let (mut s, mut e) = (p, p);
        while s.1 > 0 && class_at((s.0, s.1 - 1)) == want {
            s.1 -= 1;
        }
        while e.1 + 1 < self.cols && class_at((e.0, e.1 + 1)) == want {
            e.1 += 1;
        }
        (s, e)
    }

    /// The selected text; wrapped lines join and trailing blanks are dropped.
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        let mut out = String::new();
        let mut line = sel.start.0;
        while line <= sel.end.0 {
            let row = self.line(line);
            let from = if line == sel.start.0 { sel.start.1 } else { 0 };
            let to = if line == sel.end.0 { sel.end.1 + 1 } else { usize::MAX };
            let mut text = String::new();
            if let Some(row) = row {
                for c in row.cells.iter().take(to).skip(from) {
                    c.push_text(&mut text);
                }
            }
            let wrapped = row.is_some_and(|r| r.wrapped);
            if !wrapped {
                text.truncate(text.trim_end_matches(' ').len());
            }
            out.push_str(&text);
            if line < sel.end.0 && !wrapped {
                out.push('\n');
            }
            line += 1;
        }
        Some(out)
    }

    fn touch_selection(&mut self, row: usize) {
        if let Some(sel) = self.selection {
            let abs = self.pushed + row as u64;
            if sel.start.0 <= abs && abs <= sel.end.0 {
                self.selection = None;
            }
        }
    }

    // ---- Resizing ---------------------------------------------------------------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.selection = None;
        let blank = Cell::blank(Color::Default);

        let width_changed = cols != self.cols;
        for row in &mut self.alternate {
            row.resize(cols);
        }
        self.tabs.resize(cols, false);
        for i in self.cols..cols {
            self.tabs[i] = i % 8 == 0;
        }

        // Full-screen programs redraw themselves, so the alternate screen is only
        // cut or extended. It keeps the line its cursor is on, and text below the
        // cursor goes first.
        let mut alt_row = if self.using_alt { self.cur.row } else { self.saved[1].row }.min(self.rows - 1);
        if rows < self.rows {
            let alt_shift = (alt_row + 1).saturating_sub(rows);
            self.alternate.drain(..alt_shift);
            alt_row -= alt_shift;
        }
        self.alternate.resize(rows, Row::new(cols, &blank));
        if self.using_alt {
            self.cur.row = alt_row;
            self.cur.wrap_pending &= !width_changed;
        } else {
            self.saved[1].row = alt_row;
        }

        if width_changed {
            self.reflow_primary(cols, rows);
        } else {
            self.resize_primary_height(rows, &blank);
        }

        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.view_offset = self.view_offset.min(self.scrollback_len());
        self.cur.row = self.cur.row.min(rows - 1);
        self.cur.col = self.cur.col.min(cols - 1);
        for s in &mut self.saved {
            s.row = s.row.min(rows - 1);
            s.col = s.col.min(cols - 1);
        }
    }

    /// A height change on the primary screen. Text below the cursor goes first; what
    /// is still too much scrolls off the top, and growing brings it back.
    fn resize_primary_height(&mut self, rows: usize, blank: &Cell) {
        let saved = if self.using_alt { self.saved[0] } else { self.cur };
        let mut row = saved.row.min(self.rows - 1);
        if rows < self.rows {
            let shift = (row + 1).saturating_sub(rows);
            for r in self.primary.drain(..shift).collect::<Vec<_>>() {
                self.push_scrollback(r);
            }
            row -= shift;
        } else {
            for _ in self.rows..rows {
                let Some(mut r) = self.scrollback.pop_back() else { break };
                r.resize(self.cols);
                self.primary.insert(0, r);
                self.pushed -= 1;
                row += 1;
            }
        }
        self.primary.resize(rows, Row::new(self.cols, blank));
        if self.using_alt {
            self.saved[0].row = row;
        } else {
            self.cur.row = row;
        }
    }

    // ---- Writing text -----------------------------------------------------------

    fn translate(&mut self, c: char) -> char {
        let set = self.cur.single_shift.take().unwrap_or(self.cur.gl);
        match self.cur.charsets[set] {
            Charset::Ascii => c,
            Charset::Uk => if c == '#' { '£' } else { c },
            Charset::DecGraphics => dec_graphics(c),
        }
    }

    fn print(&mut self, c: char) {
        let c = self.translate(c);
        let width = char_width(c);
        if width == 0 {
            self.add_combining(c);
            return;
        }
        self.last_printed = Some(c);
        if self.cur.wrap_pending && self.cur.autowrap {
            self.wrap();
        }
        if width > self.cols {
            return;
        }
        if width == 2 && self.cur.col + 1 >= self.cols {
            if self.cur.autowrap {
                let (row, col) = (self.cur.row, self.cur.col);
                let mut pad = Cell::blank(self.cur.pen.bg);
                pad.flags = flag::WRAP_PAD;
                self.split_wide(row, col);
                self.screen()[row].cells[col] = pad;
                self.wrap();
            } else {
                self.cur.col = self.cols - 2;
            }
        }
        let (row, col) = (self.cur.row, self.cur.col);
        self.touch_selection(row);
        if self.insert_mode {
            self.insert_cells(width);
        }
        self.split_wide(row, col);
        if width == 2 {
            self.split_wide(row, col + 1);
        }

        let pen = self.cur.pen;
        let extra = pen.underline_color.map(|u| {
            Box::new(Extra { underline_color: Some(u), ..Extra::default() })
        });
        let cell = Cell { c, fg: pen.fg, bg: pen.bg, flags: pen.flags, extra };
        let cells = &mut self.screen()[row].cells;
        if width == 2 {
            let mut cont = cell.clone();
            cont.c = ' ';
            cont.flags |= flag::WIDE_CONT;
            cells[col + 1] = cont;
        }
        cells[col] = cell;

        if col + width >= self.cols {
            self.cur.col = self.cols - 1;
            self.cur.wrap_pending = true;
        } else {
            self.cur.col = col + width;
            self.cur.wrap_pending = false;
        }
    }

    fn wrap(&mut self) {
        let row = self.cur.row;
        self.screen()[row].wrapped = true;
        self.cur.col = 0;
        self.cur.wrap_pending = false;
        self.index();
    }

    fn add_combining(&mut self, c: char) {
        let (row, mut col) = (self.cur.row, self.cur.col);
        if !self.cur.wrap_pending {
            if col == 0 {
                return;
            }
            col -= 1;
        }
        let cells = &mut self.screen()[row].cells;
        if cells[col].is_wide_cont() && col > 0 {
            col -= 1;
        }
        let extra = cells[col].extra.get_or_insert_with(Default::default);
        // Bound the text attached to one cell, however much is thrown at it.
        if extra.combining.len() < 32 {
            extra.combining.push(c);
        }
    }

    /// If (row, col) is one half of a double-width character, blank the other half.
    fn split_wide(&mut self, row: usize, col: usize) {
        let cols = self.cols;
        let cells = &mut self.screen()[row].cells;
        if col >= cols {
            return;
        }
        if cells[col].is_wide_cont() {
            if col > 0 {
                cells[col - 1] = Cell::blank(cells[col - 1].bg);
            }
            cells[col] = Cell::blank(cells[col].bg);
        } else if col + 1 < cols && cells[col + 1].is_wide_cont() {
            cells[col + 1] = Cell::blank(cells[col + 1].bg);
            cells[col] = Cell::blank(cells[col].bg);
        }
    }

    // ---- Cursor movement --------------------------------------------------------

    fn goto(&mut self, row: usize, col: usize) {
        let (min, max) = if self.cur.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        let row = if self.cur.origin { row.saturating_add(self.top) } else { row };
        self.cur.row = row.clamp(min, max);
        self.cur.col = col.min(self.cols - 1);
        self.cur.wrap_pending = false;
    }

    fn move_up(&mut self, n: usize) {
        let min = if self.cur.row >= self.top { self.top } else { 0 };
        self.cur.row = self.cur.row.saturating_sub(n).max(min);
        self.cur.wrap_pending = false;
    }

    fn move_down(&mut self, n: usize) {
        let max = if self.cur.row <= self.bottom { self.bottom } else { self.rows - 1 };
        self.cur.row = self.cur.row.saturating_add(n).min(max);
        self.cur.wrap_pending = false;
    }

    fn move_left(&mut self, n: usize) {
        self.cur.col = self.cur.col.saturating_sub(n);
        self.cur.wrap_pending = false;
    }

    fn move_right(&mut self, n: usize) {
        self.cur.col = self.cur.col.saturating_add(n).min(self.cols - 1);
        self.cur.wrap_pending = false;
    }

    fn index(&mut self) {
        if self.cur.row == self.bottom {
            self.scroll_up(1);
        } else if self.cur.row + 1 < self.rows {
            self.cur.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cur.row == self.top {
            self.scroll_down(1);
        } else if self.cur.row > 0 {
            self.cur.row -= 1;
        }
    }

    fn tab(&mut self, n: usize) {
        for _ in 0..n {
            let next = (self.cur.col + 1..self.cols).find(|&i| self.tabs[i]);
            self.cur.col = next.unwrap_or(self.cols - 1);
        }
        self.cur.wrap_pending = false;
    }

    fn back_tab(&mut self, n: usize) {
        for _ in 0..n {
            let prev = (0..self.cur.col).rev().find(|&i| self.tabs[i]);
            self.cur.col = prev.unwrap_or(0);
        }
        self.cur.wrap_pending = false;
    }

    // ---- Scrolling --------------------------------------------------------------

    fn push_scrollback(&mut self, mut row: Row) {
        if self.max_scrollback == 0 {
            // Line numbers still advance, to keep selections on their text.
            self.pushed += 1;
            return;
        }
        row.trim();
        self.scrollback.push_back(row);
        self.pushed += 1;
        if self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        } else if self.view_offset > 0 {
            // Hold the view still while new text arrives below it.
            self.view_offset += 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let blank = self.blank();
        let cols = self.cols;
        if top == 0 && !self.using_alt {
            for i in 0..n {
                let row = std::mem::replace(&mut self.primary[i], Row::new(cols, &blank));
                self.push_scrollback(row);
            }
            self.primary[..=bottom].rotate_left(n);
        } else {
            let screen = self.screen();
            screen[top..=bottom].rotate_left(n);
            for row in &mut screen[bottom + 1 - n..=bottom] {
                row.fill(&blank);
            }
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let blank = self.blank();
        let screen = self.screen();
        screen[top..=bottom].rotate_right(n);
        for row in &mut screen[top..top + n] {
            row.fill(&blank);
        }
    }

    // ---- Editing ----------------------------------------------------------------

    fn in_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cur.row)
    }

    fn insert_lines(&mut self, n: usize) {
        if !self.in_region() {
            return;
        }
        let saved = self.top;
        self.top = self.cur.row;
        self.scroll_down(n);
        self.top = saved;
        self.cur.col = 0;
        self.cur.wrap_pending = false;
    }

    fn delete_lines(&mut self, n: usize) {
        if !self.in_region() {
            return;
        }
        // Deleted lines are gone rather than scrolled away, so they skip the scrollback.
        let blank = self.blank();
        let (top, bottom) = (self.cur.row, self.bottom);
        let n = n.min(bottom - top + 1);
        let screen = self.screen();
        screen[top..=bottom].rotate_left(n);
        for row in &mut screen[bottom + 1 - n..=bottom] {
            row.fill(&blank);
        }
        self.cur.col = 0;
        self.cur.wrap_pending = false;
    }

    fn insert_cells(&mut self, n: usize) {
        let (row, col, cols) = (self.cur.row, self.cur.col, self.cols);
        let n = n.min(cols - col);
        let blank = self.blank();
        self.split_wide(row, col);
        self.split_wide(row, cols - n);
        let cells = &mut self.screen()[row].cells;
        cells[col..].rotate_right(n);
        cells[col..col + n].fill(blank);
        self.cur.wrap_pending = false;
    }

    fn delete_cells(&mut self, n: usize) {
        let (row, col, cols) = (self.cur.row, self.cur.col, self.cols);
        let n = n.min(cols - col);
        let blank = self.blank();
        self.split_wide(row, col);
        self.split_wide(row, (col + n).min(cols - 1));
        let cells = &mut self.screen()[row].cells;
        cells[col..].rotate_left(n);
        cells[cols - n..].fill(blank);
        self.cur.wrap_pending = false;
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize, selective: bool) {
        let to = to.min(self.cols);
        if from >= to {
            return;
        }
        let blank = self.blank();
        self.touch_selection(row);
        self.split_wide(row, from);
        self.split_wide(row, to - 1);
        let r = &mut self.screen()[row];
        for c in &mut r.cells[from..to] {
            if !(selective && c.flags & flag::PROTECTED != 0) {
                *c = blank.clone();
            }
        }
        r.wrapped = false;
    }

    fn erase_in_display(&mut self, mode: u16, selective: bool) {
        let (row, col, rows, cols) = (self.cur.row, self.cur.col, self.rows, self.cols);
        match mode {
            0 => {
                self.erase_cells(row, col, cols, selective);
                (row + 1..rows).for_each(|r| self.erase_cells(r, 0, cols, selective));
            }
            1 => {
                (0..row).for_each(|r| self.erase_cells(r, 0, cols, selective));
                self.erase_cells(row, 0, col + 1, selective);
            }
            2 => (0..rows).for_each(|r| self.erase_cells(r, 0, cols, selective)),
            3 if !self.using_alt => self.clear_scrollback(),
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16, selective: bool) {
        let (row, col, cols) = (self.cur.row, self.cur.col, self.cols);
        match mode {
            0 => self.erase_cells(row, col, cols, selective),
            1 => self.erase_cells(row, 0, col + 1, selective),
            2 => self.erase_cells(row, 0, cols, selective),
            _ => {}
        }
    }

    // ---- Screen switching and reset ---------------------------------------------

    fn save_cursor(&mut self) {
        self.saved[self.using_alt as usize] = self.cur;
    }

    fn restore_cursor(&mut self) {
        self.cur = self.saved[self.using_alt as usize];
        self.cur.row = self.cur.row.min(self.rows - 1);
        self.cur.col = self.cur.col.min(self.cols - 1);
    }

    fn set_alt_screen(&mut self, on: bool) {
        if self.using_alt != on {
            self.using_alt = on;
            self.selection = None;
            self.view_offset = 0;
        }
    }

    fn clear_alt_screen(&mut self) {
        let blank = self.blank();
        self.alternate.iter_mut().for_each(|r| r.fill(&blank));
    }

    /// RIS: back to the power-on state. The history is kept, as in xterm.
    pub fn reset(&mut self) {
        let fresh = Terminal::new(self.cols, self.rows, self.max_scrollback, self.theme_colors.clone());
        let scrollback = std::mem::take(&mut self.scrollback);
        let (pushed, title, cell_w, cell_h) = (self.pushed, std::mem::take(&mut self.title), self.cell_w, self.cell_h);
        let replies = std::mem::take(&mut self.replies);
        let new_title = self.new_title.take();
        let parser = std::mem::replace(&mut self.parser, Parser::new());
        *self = fresh;
        self.scrollback = scrollback;
        self.pushed = pushed;
        self.title = title;
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        self.replies = replies;
        self.new_title = new_title;
        self.parser = parser;
    }

    /// DECSTR: reset modes but keep the screen.
    fn soft_reset(&mut self) {
        self.cursor_visible = true;
        self.insert_mode = false;
        self.app_cursor = false;
        self.app_keypad = false;
        self.cur.origin = false;
        self.cur.autowrap = true;
        self.cur.pen = Pen::new();
        self.cur.charsets = [Charset::Ascii; 4];
        self.cur.gl = 0;
        self.cur.single_shift = None;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.saved = [Cursor::new(); 2];
    }

    // ---- Graphics ---------------------------------------------------------------

    fn place_image(&mut self, width: u32, height: u32, rgba: Vec<u8>) {
        let (cw, ch) = (self.cell_w.max(1), self.cell_h.max(1));
        let image = Rc::new(Image {
            width,
            height,
            rgba: glib::Bytes::from_owned(rgba),
            cell_w: cw,
            cell_h: ch,
            texture: Default::default(),
        });
        let tiles_w = width.div_ceil(cw) as usize;
        let tiles_h = height.div_ceil(ch) as usize;
        let saved = self.cur;
        if !self.sixel_scrolling {
            self.cur.row = 0;
            self.cur.col = 0;
        }
        let left = self.cur.col;
        self.cur.wrap_pending = false;
        for ty in 0..tiles_h {
            let row = self.cur.row;
            self.touch_selection(row);
            for tx in 0..tiles_w.min(self.cols - left) {
                let col = left + tx;
                self.split_wide(row, col);
                let tile = Tile { image: image.clone(), col: tx as u16, row: ty as u16 };
                let mut cell = Cell::blank(self.cur.pen.bg);
                cell.extra = Some(Box::new(Extra { tile: Some(tile), ..Extra::default() }));
                self.screen()[row].cells[col] = cell;
            }
            if ty + 1 == tiles_h {
                break;
            }
            if self.sixel_scrolling {
                self.index();
            } else if row + 1 < self.rows {
                self.cur.row += 1;
            } else {
                break;
            }
        }
        if !self.sixel_scrolling {
            // Display mode: the image does not move the cursor.
            self.cur = saved;
        } else if self.sixel_cursor_right && left + tiles_w < self.cols {
            self.cur.col = left + tiles_w;
        } else {
            self.index();
            self.cur.col = left;
        }
    }
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|i| i % 8 == 0).collect()
}

fn dec_graphics(c: char) -> char {
    const TABLE: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
    match c {
        '`'..='~' => TABLE.chars().nth(c as usize - 0x60).unwrap_or(c),
        _ => c,
    }
}
