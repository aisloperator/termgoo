//! Rewrapping text when the terminal changes width.
//!
//! A logical line is a run of rows joined by autowrap. Changing the width takes
//! the whole primary screen and scrollback apart into logical lines and lays them
//! out again, while anchors keep the cursor and the view on the text they were on.

use super::cell::{Cell, Color, Row, char_width, flag};
use super::{Cursor, Terminal};

/// A place in the old rows, as (row, column).
type Place = (usize, usize);

const CURSOR: usize = 0;
const SAVED: usize = 1;
const SCREEN_TOP: usize = 2;
const VIEW_TOP: usize = 3;
const ANCHORS: usize = 4;

/// The column a cursor really stands at, which with a wrap pending is past the last cell.
fn logical_col(c: &Cursor) -> usize {
    c.col + c.wrap_pending as usize
}

/// Lay out one logical line. `offsets` are (anchor, offset into `cells`) pairs.
fn wrap_line(cells: Vec<Cell>, cols: usize, out: &mut Vec<Row>, offsets: &[(usize, usize)], moved: &mut [Option<Place>; ANCHORS]) {
    let mut cells = cells.into_iter().peekable();
    let mut consumed = 0;
    loop {
        let row_start = consumed;
        let mut row = Vec::with_capacity(cols);
        while row.len() < cols {
            let Some(cell) = cells.peek() else { break };
            if char_width(cell.c) == 2 && !cell.is_wide_cont() {
                if cols < 2 {
                    // Not even one row can hold it.
                    let bg = cell.bg;
                    cells.next();
                    cells.next();
                    consumed += 2;
                    row.push(Cell::blank(bg));
                    continue;
                }
                if row.len() + 2 > cols {
                    let mut pad = Cell::blank(Color::Default);
                    pad.flags = flag::WRAP_PAD;
                    row.push(pad);
                    break;
                }
            }
            row.extend(cells.next());
            consumed += 1;
        }
        let more = cells.peek().is_some();
        for &(anchor, offset) in offsets {
            if offset >= row_start && (offset < consumed || !more) {
                moved[anchor] = Some((out.len(), offset - row_start));
            }
        }
        out.push(Row { cells: row, wrapped: more });
        if !more {
            return;
        }
    }
}

impl Terminal {
    /// Rewrap the primary screen and the scrollback to `cols`, for a screen of `rows`.
    pub(super) fn reflow_primary(&mut self, cols: usize, rows: usize) {
        let first_screen_row = self.scrollback.len();
        let mut old: Vec<Row> = self.scrollback.drain(..).collect();
        old.append(&mut self.primary);

        let cursor = if self.using_alt { self.saved[0] } else { self.cur };
        let saved = self.saved[0];
        let last_row = self.rows - 1;
        let places: [Place; ANCHORS] = [
            (first_screen_row + cursor.row.min(last_row), logical_col(&cursor)),
            (first_screen_row + saved.row.min(last_row), logical_col(&saved)),
            (first_screen_row, 0),
            (first_screen_row - self.view_offset.min(first_screen_row), 0),
        ];

        // Blank rows below the text and the cursor are not part of anything.
        let content_end = old.iter().rposition(|r| r.wrapped || r.content_len() > 0).map_or(0, |i| i + 1);
        let end = content_end.max(places[CURSOR].0 + 1);

        let mut out: Vec<Row> = Vec::with_capacity(end);
        let mut moved: [Option<Place>; ANCHORS] = [None; ANCHORS];
        let mut i = 0;
        while i < end {
            let mut j = i;
            while j + 1 < end && old[j].wrapped {
                j += 1;
            }
            let anchored = places.iter().any(|p| (i..=j).contains(&p.0));
            let has_image = old[i..=j].iter().any(|r| r.cells.iter().any(|c| c.tile().is_some()));

            if has_image {
                // Tiles only mean something in their place, so these rows stay whole.
                for (k, row) in old.iter_mut().enumerate().take(j + 1).skip(i) {
                    for (anchor, place) in places.iter().enumerate() {
                        if place.0 == k {
                            moved[anchor] = Some((out.len(), place.1));
                        }
                    }
                    let mut row = Row { cells: std::mem::take(&mut row.cells), wrapped: row.wrapped };
                    row.resize(cols);
                    out.push(row);
                }
            } else if i == j && !anchored && old[i].content_len() <= cols {
                // Most lines. Moving the row is much cheaper than laying it out.
                let len = old[i].content_len();
                let mut cells = std::mem::take(&mut old[i].cells);
                cells.truncate(len);
                out.push(Row { cells, wrapped: false });
            } else {
                let mut line = Vec::new();
                let mut offsets = Vec::new();
                for (k, row) in old.iter_mut().enumerate().take(j + 1).skip(i) {
                    let len = row.content_len();
                    let mut cells = std::mem::take(&mut row.cells);
                    if k == j {
                        cells.truncate(len);
                    }
                    cells.retain(|c| !c.is_wrap_pad());
                    for (anchor, place) in places.iter().enumerate() {
                        if place.0 == k {
                            let col = if k == j { place.1 } else { place.1.min(cells.len()) };
                            offsets.push((anchor, line.len() + col));
                        }
                    }
                    line.append(&mut cells);
                }
                // Blanks before the cursor are text the user is about to continue,
                // such as the space after a prompt.
                if let Some(&(_, offset)) = offsets.iter().find(|(anchor, _)| *anchor == CURSOR)
                    && offset > line.len()
                {
                    line.resize(offset, Cell::blank(Color::Default));
                }
                wrap_line(line, cols, &mut out, &offsets, &mut moved);
            }
            i = j + 1;
        }

        let place_of = |anchor: usize| moved[anchor].unwrap_or((out.len() - 1, 0));
        let (cursor_row, cursor_col) = place_of(CURSOR);

        // The screen stays on the text it was showing, unless the cursor or the
        // text below it would then fall off the bottom.
        let mut top = place_of(SCREEN_TOP).0;
        if out.len() > top + rows {
            top = (out.len() - rows).min(cursor_row);
        }
        top = top.max((cursor_row + 1).saturating_sub(rows)).min(cursor_row);

        let view_top = place_of(VIEW_TOP).0;
        self.view_offset = if self.view_offset > 0 { top.saturating_sub(view_top) } else { 0 };
        let (saved_row, saved_col) = place_of(SAVED);

        let mut screen = out.split_off(top);
        screen.truncate(rows);
        for row in &mut screen {
            row.resize(cols);
        }
        screen.resize(rows, Row::new(cols, &Cell::blank(Color::Default)));
        self.primary = screen;

        self.pushed = self.pushed - first_screen_row as u64 + out.len() as u64;
        self.scrollback = out.into();
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }

        let cursor = if self.using_alt { &mut self.saved[0] } else { &mut self.cur };
        cursor.row = cursor_row - top;
        cursor.wrap_pending = cursor_col >= cols;
        cursor.col = cursor_col.min(cols - 1);
        if !self.using_alt {
            self.saved[0].row = saved_row.saturating_sub(top);
            self.saved[0].wrap_pending = saved_col >= cols;
            self.saved[0].col = saved_col.min(cols - 1);
        }
    }
}
