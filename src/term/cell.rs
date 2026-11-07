//! The contents of one character cell, and rows of them.

use gtk::glib;
use std::cell::OnceCell;
use std::rc::Rc;

/// Attribute bits for `Cell::flags`.
pub mod flag {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const BLINK: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const HIDDEN: u16 = 1 << 5;
    pub const STRIKE: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7;
    /// Selective erase (DECSCA) leaves this cell alone.
    pub const PROTECTED: u16 = 1 << 8;
    /// Right half of a double-width character; holds no text of its own.
    pub const WIDE_CONT: u16 = 1 << 9;
    /// Blank left at the end of a wrapped row because a double-width character
    /// did not fit. It is layout, not text, so rewrapping and copying skip it.
    pub const WRAP_PAD: u16 = 1 << 13;
    /// Underline style lives in these 3 bits; see the `UL_*` values.
    pub const UL_SHIFT: u16 = 10;
    pub const UL_MASK: u16 = 7 << UL_SHIFT;
}

pub const UL_NONE: u16 = 0;
pub const UL_SINGLE: u16 = 1;
pub const UL_DOUBLE: u16 = 2;
pub const UL_CURLY: u16 = 3;
pub const UL_DOTTED: u16 = 4;
pub const UL_DASHED: u16 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// A decoded Sixel image. Tiles of it are stored in cells, so that scrolling,
/// erasing and overwriting treat graphics exactly like text.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: glib::Bytes,
    /// Cell size when the image was decoded, which fixes how tiles map to pixels.
    pub cell_w: u32,
    pub cell_h: u32,
    /// Lazily built by the renderer.
    pub texture: OnceCell<gtk::gdk::Texture>,
}

#[derive(Clone)]
pub struct Tile {
    pub image: Rc<Image>,
    pub col: u16,
    pub row: u16,
}

/// Rarely used cell data, kept out of line so that `Cell` stays small.
#[derive(Clone, Default)]
pub struct Extra {
    pub combining: String,
    pub tile: Option<Tile>,
    pub underline_color: Option<Color>,
}

#[derive(Clone)]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
    pub extra: Option<Box<Extra>>,
}

impl Cell {
    pub fn blank(bg: Color) -> Cell {
        Cell { c: ' ', fg: Color::Default, bg, flags: 0, extra: None }
    }

    pub fn underline(&self) -> u16 {
        (self.flags & flag::UL_MASK) >> flag::UL_SHIFT
    }

    pub fn is_wide_cont(&self) -> bool {
        self.flags & flag::WIDE_CONT != 0
    }

    pub fn is_wrap_pad(&self) -> bool {
        self.flags & flag::WRAP_PAD != 0
    }

    pub fn tile(&self) -> Option<&Tile> {
        self.extra.as_ref()?.tile.as_ref()
    }

    /// True if dropping this cell loses nothing (used to trim scrollback rows).
    fn is_plain_blank(&self) -> bool {
        self.c == ' ' && self.bg == Color::Default && self.flags == 0 && self.extra.is_none()
    }

    pub fn push_text(&self, out: &mut String) {
        if self.is_wide_cont() || self.is_wrap_pad() {
            return;
        }
        out.push(self.c);
        if let Some(e) = &self.extra {
            out.push_str(&e.combining);
        }
    }
}

#[derive(Clone)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// This row continues on the next one because of autowrap, so copying
    /// a selection across the boundary does not insert a newline.
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, blank: &Cell) -> Row {
        Row { cells: vec![blank.clone(); cols], wrapped: false }
    }

    pub fn fill(&mut self, blank: &Cell) {
        self.cells.fill(blank.clone());
        self.wrapped = false;
    }

    /// How many cells are left once trailing blanks are ignored.
    pub fn content_len(&self) -> usize {
        self.cells.iter().rposition(|c| !c.is_plain_blank()).map_or(0, |i| i + 1)
    }

    /// Drop trailing blank cells, to save memory in the scrollback. In a wrapped
    /// row they are real spaces in the middle of a line, so they stay.
    pub fn trim(&mut self) {
        if !self.wrapped {
            self.cells.truncate(self.content_len());
            self.cells.shrink_to_fit();
        }
    }

    pub fn resize(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::blank(Color::Default));
        // Do not leave half of a double-width character at the new edge.
        if let Some(last) = self.cells.last_mut().filter(|c| char_width(c.c) == 2) {
            *last = Cell::blank(Color::Default);
        }
    }
}

/// Display width of a character in cells: 0, 1 or 2.
/// GLib's Unicode tables are already linked in, so this needs no extra crate.
pub fn char_width(c: char) -> usize {
    if c.is_ascii() {
        return 1;
    }
    let u = c as u32;
    // SAFETY: both functions accept any code point and only read static tables.
    unsafe {
        if glib::ffi::g_unichar_iszerowidth(u) != 0 {
            0
        } else if glib::ffi::g_unichar_iswide(u) != 0 {
            2
        } else {
            1
        }
    }
}
