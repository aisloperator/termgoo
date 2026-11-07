use super::*;
use crate::config::Palette;

fn term(cols: usize, rows: usize) -> Terminal {
    Terminal::new(cols, rows, 100, Palette::dark().theme_colors())
}

fn feed(t: &mut Terminal, s: &str) {
    t.feed(s.as_bytes());
}

fn line(t: &Terminal, y: usize) -> String {
    let mut s = String::new();
    t.display_row(y).cells.iter().for_each(|c| c.push_text(&mut s));
    s.trim_end().to_string()
}

fn cursor(t: &Terminal) -> (usize, usize) {
    (t.cur.row, t.cur.col)
}

fn replies(t: &mut Terminal) -> String {
    String::from_utf8_lossy(&std::mem::take(&mut t.replies)).into_owned()
}

fn cell(t: &Terminal, y: usize, x: usize) -> &Cell {
    &t.display_row(y).cells[x]
}

#[test]
fn text_and_control_characters() {
    let mut t = term(10, 3);
    feed(&mut t, "ab\r\ncd\tx\x08y");
    assert_eq!(line(&t, 0), "ab");
    assert_eq!(line(&t, 1), "cd      y");
    assert_eq!(cursor(&t), (1, 9));
}

#[test]
fn autowrap_waits_for_the_next_character() {
    let mut t = term(3, 2);
    feed(&mut t, "abc");
    assert_eq!(cursor(&t), (0, 2));
    feed(&mut t, "d");
    assert_eq!(line(&t, 1), "d");
    assert!(t.display_row(0).wrapped);

    let mut t = term(3, 2);
    feed(&mut t, "\x1b[?7labcdef");
    assert_eq!(line(&t, 0), "abf");
    assert_eq!(line(&t, 1), "");
}

#[test]
fn cursor_movement_and_clamping() {
    let mut t = term(10, 5);
    feed(&mut t, "\x1b[3;4H");
    assert_eq!(cursor(&t), (2, 3));
    feed(&mut t, "\x1b[A\x1b[2C\x1b[B\x1b[D");
    assert_eq!(cursor(&t), (2, 4));
    feed(&mut t, "\x1b[99;99H");
    assert_eq!(cursor(&t), (4, 9));
    feed(&mut t, "\x1b[H\x1b[5G\x1b[3d");
    assert_eq!(cursor(&t), (2, 4));
}

#[test]
fn erasing_uses_the_current_background() {
    let mut t = term(5, 2);
    feed(&mut t, "abcde\x1b[1;3H\x1b[41m\x1b[K");
    assert_eq!(line(&t, 0), "ab");
    assert_eq!(cell(&t, 0, 4).bg, Color::Indexed(1));
    assert_eq!(cell(&t, 0, 1).bg, Color::Default);
    feed(&mut t, "\x1b[m\x1b[2J");
    assert_eq!(line(&t, 0), "");
}

#[test]
fn insert_delete_characters_and_lines() {
    let mut t = term(6, 3);
    feed(&mut t, "abcdef\x1b[1;2H\x1b[2@");
    assert_eq!(line(&t, 0), "a  bcd");
    feed(&mut t, "\x1b[3P");
    assert_eq!(line(&t, 0), "acd");
    feed(&mut t, "\x1b[2;1H1\x1b[3;1H2\x1b[2;1H\x1b[L");
    assert_eq!(line(&t, 1), "");
    assert_eq!(line(&t, 2), "1");
    feed(&mut t, "\x1b[M");
    assert_eq!(line(&t, 1), "1");
}

#[test]
fn insert_mode() {
    let mut t = term(5, 1);
    feed(&mut t, "abc\x1b[1;1H\x1b[4hX\x1b[4lY");
    assert_eq!(line(&t, 0), "XYbc");
}

#[test]
fn scroll_region_leaves_the_rest_alone() {
    let mut t = term(5, 5);
    feed(&mut t, "1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[4;1H\n");
    assert_eq!((0..5).map(|y| line(&t, y)).collect::<Vec<_>>(), ["1", "3", "4", "", "5"]);
    assert_eq!(t.scrollback_len(), 0);
    feed(&mut t, "\x1b[2;1H\x1bM");
    assert_eq!((0..5).map(|y| line(&t, y)).collect::<Vec<_>>(), ["1", "", "3", "4", "5"]);
}

#[test]
fn origin_mode_is_relative_to_the_region() {
    let mut t = term(5, 5);
    feed(&mut t, "\x1b[2;4r\x1b[?6h\x1b[H");
    assert_eq!(cursor(&t), (1, 0));
    feed(&mut t, "\x1b[99;1H");
    assert_eq!(cursor(&t), (3, 0));
    feed(&mut t, "\x1b[6n");
    assert_eq!(replies(&mut t), "\x1b[3;1R");
}

#[test]
fn scrollback_collects_and_is_bounded() {
    let mut t = Terminal::new(5, 2, 3, Palette::dark().theme_colors());
    for i in 0..10 {
        feed(&mut t, &format!("{i}\r\n"));
    }
    assert_eq!(t.scrollback_len(), 3);
    t.scroll_view(3);
    assert_eq!(line(&t, 0), "6");
    assert_eq!(t.cursor_pos(), None);
    t.scroll_view(-3);
    assert_eq!(line(&t, 0), "9");
}

#[test]
fn view_stays_put_while_output_arrives() {
    let mut t = term(5, 2);
    for i in 0..5 {
        feed(&mut t, &format!("{i}\r\n"));
    }
    t.scroll_view(2);
    let before = line(&t, 0);
    feed(&mut t, "x\r\ny\r\n");
    assert_eq!(line(&t, 0), before);
}

#[test]
fn zero_scrollback_keeps_nothing() {
    let mut t = Terminal::new(5, 2, 0, Palette::dark().theme_colors());
    feed(&mut t, "1\r\n2\r\n3\r\n");
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn clear_scrollback_with_ed3() {
    let mut t = term(5, 2);
    feed(&mut t, "1\r\n2\r\n3\r\n\x1b[3J");
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn alternate_screen_is_separate_and_restores_the_cursor() {
    let mut t = term(5, 2);
    feed(&mut t, "main\x1b[?1049h");
    assert!(t.using_alt);
    assert_eq!(line(&t, 0), "");
    feed(&mut t, "alt\r\n\r\n\r\n");
    assert_eq!(t.scrollback_len(), 0);
    feed(&mut t, "\x1b[?1049l");
    assert_eq!(line(&t, 0), "main");
    assert_eq!(cursor(&t), (0, 4));
}

#[test]
fn save_and_restore_cursor() {
    let mut t = term(10, 5);
    feed(&mut t, "\x1b[2;3H\x1b[31m\x1b7\x1b[H\x1b[m\x1b8x");
    assert_eq!(cursor(&t), (1, 3));
    assert_eq!(cell(&t, 1, 2).fg, Color::Indexed(1));
}

#[test]
fn sgr_attributes_and_colors() {
    let mut t = term(20, 1);
    feed(&mut t, "\x1b[1;3;4;7;31;42mA\x1b[0m");
    let c = cell(&t, 0, 0);
    assert_eq!(c.flags & flag::BOLD, flag::BOLD);
    assert_eq!(c.flags & flag::ITALIC, flag::ITALIC);
    assert_eq!(c.flags & flag::INVERSE, flag::INVERSE);
    assert_eq!(c.underline(), cell::UL_SINGLE);
    assert_eq!((c.fg, c.bg), (Color::Indexed(1), Color::Indexed(2)));

    feed(&mut t, "\x1b[92;103mB\x1b[38;5;200;48;2;1;2;3mC\x1b[38:2::9:8:7;48:5:17mD\x1b[4:3;58;5;9mE\x1b[24;59mF");
    assert_eq!((cell(&t, 0, 1).fg, cell(&t, 0, 1).bg), (Color::Indexed(10), Color::Indexed(11)));
    assert_eq!((cell(&t, 0, 2).fg, cell(&t, 0, 2).bg), (Color::Indexed(200), Color::Rgb(1, 2, 3)));
    assert_eq!((cell(&t, 0, 3).fg, cell(&t, 0, 3).bg), (Color::Rgb(9, 8, 7), Color::Indexed(17)));
    assert_eq!(cell(&t, 0, 4).underline(), cell::UL_CURLY);
    assert_eq!(cell(&t, 0, 4).extra.as_ref().unwrap().underline_color, Some(Color::Indexed(9)));
    assert_eq!(cell(&t, 0, 5).underline(), cell::UL_NONE);
}

#[test]
fn unicode_wide_and_combining() {
    let mut t = term(6, 2);
    feed(&mut t, "e\u{301}漢x");
    assert_eq!(line(&t, 0), "e\u{301}漢x");
    assert_eq!(cursor(&t), (0, 4));
    assert!(cell(&t, 0, 2).is_wide_cont());

    // Overwriting half of a wide character blanks the other half.
    feed(&mut t, "\x1b[1;3HZ");
    assert_eq!(line(&t, 0), "e\u{301} Zx");
    feed(&mut t, "\x1b[1;2H漢\x1b[1;2HQ");
    assert_eq!(line(&t, 0), "e\u{301}Q x");
}

#[test]
fn wide_character_wraps_whole() {
    let mut t = term(3, 2);
    feed(&mut t, "ab漢");
    assert_eq!(line(&t, 0), "ab");
    assert_eq!(line(&t, 1), "漢");
}

#[test]
fn utf8_split_across_reads_and_invalid_input() {
    let mut t = term(10, 2);
    let bytes = "é漢😀".as_bytes();
    for b in bytes {
        t.feed(&[*b]);
    }
    assert_eq!(line(&t, 0), "é漢😀");
    feed(&mut t, "\r\n");
    t.feed(&[0xff, b'a', 0xc3, b'b', 0xc0, 0x80]);
    assert!(line(&t, 1).contains('a') && line(&t, 1).contains('b'));
    assert!(line(&t, 1).contains('\u{fffd}'));
}

#[test]
fn sequences_split_across_reads() {
    let mut t = term(10, 2);
    for b in b"\x1b[2;3Hx\x1b]2;title\x07" {
        t.feed(&[*b]);
    }
    assert_eq!(line(&t, 1), "  x");
    assert_eq!(t.new_title.as_deref(), Some("title"));
}

#[test]
fn dec_line_drawing_charset() {
    let mut t = term(10, 1);
    feed(&mut t, "\x1b(0lqk\x1b(Bq\x0e\x0fq\x1b)0\x0eq\x0fq");
    assert_eq!(line(&t, 0), "┌─┐qq─q");
}

#[test]
fn title_commands_and_stack() {
    let mut t = term(10, 1);
    feed(&mut t, "\x1b]0;one\x1b\\");
    assert_eq!(t.new_title.take().as_deref(), Some("one"));
    feed(&mut t, "\x1b[22t\x1b]2;two\x07\x1b[23t");
    assert_eq!(t.new_title.take().as_deref(), Some("one"));
    feed(&mut t, "\x1b]2;a\x7fb\x07");
    assert_eq!(t.new_title.take().as_deref(), Some("ab"));
}

#[test]
fn device_reports() {
    let mut t = term(80, 24);
    feed(&mut t, "\x1b[c");
    assert!(replies(&mut t).split(';').any(|p| p == "4"), "sixel is announced");
    feed(&mut t, "\x1b[5n\x1b[18t\x1b[14t");
    assert_eq!(replies(&mut t), format!("\x1b[0n\x1b[8;24;80t\x1b[4;{};{}t", 24 * t.cell_h, 80 * t.cell_w));
    feed(&mut t, "\x1b[>q");
    assert!(replies(&mut t).contains("termgoo"));
    feed(&mut t, "\x1b[?25$p\x1b[?9999$p\x1b[?2004h\x1b[?2004$p");
    assert_eq!(replies(&mut t), "\x1b[?25;1$y\x1b[?9999;0$y\x1b[?2004;1$y");
    feed(&mut t, "\x1bP$qm\x1b\\\x1bP$qzz\x1b\\");
    assert_eq!(replies(&mut t), "\x1bP1$r0m\x1b\\\x1bP0$r\x1b\\");
}

#[test]
fn osc_colors() {
    let mut t = term(10, 1);
    feed(&mut t, "\x1b]4;1;#102030\x07\x1b]4;1;?\x07");
    assert_eq!(replies(&mut t), "\x1b]4;1;rgb:1010/2020/3030\x1b\\");
    feed(&mut t, "\x1b]104\x07\x1b]11;rgb:ff/00/00\x07\x1b]11;?\x07");
    assert_eq!(replies(&mut t), "\x1b]11;rgb:ffff/0000/0000\x1b\\");
    feed(&mut t, "\x1b]111\x07");
    assert_eq!(t.bg_override, None);
}

#[test]
fn programs_cannot_remap_keys() {
    let mut t = term(40, 2);
    // DECUDK, then ANSI.SYS reassignment, then xterm modifyOtherKeys and resources.
    feed(&mut t, "\x1bP0;1|17/72656D6F7665\x1b\\");
    feed(&mut t, "\x1b[65;66p\x1b[0;59;114;109;13p");
    feed(&mut t, "\x1b[>4;2m\x1b[>1u\x1bP+p7365\x1b\\");
    assert_eq!(replies(&mut t), "");
    assert_eq!(line(&t, 0), "", "nothing leaks onto the screen");
    feed(&mut t, "ok");
    assert_eq!(line(&t, 0), "ok", "the parser recovers");
    assert_eq!(cell(&t, 0, 0).flags, 0, "XTMODKEYS is not read as SGR");
}

#[test]
fn clipboard_escape_is_ignored() {
    let mut t = term(10, 1);
    feed(&mut t, "\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07x");
    assert_eq!(replies(&mut t), "");
    assert_eq!(line(&t, 0), "x");
}

#[test]
fn application_cannot_resize_the_window() {
    let mut t = term(10, 5);
    feed(&mut t, "\x1b[8;50;200t\x1b[?3h");
    assert_eq!((t.cols, t.rows), (10, 5));
}

#[test]
fn hostile_parameters_do_not_panic() {
    let mut t = term(5, 3);
    feed(&mut t, "\x1b[99999;99999H\x1b[99999@\x1b[99999P\x1b[99999L\x1b[99999M\x1b[99999S\x1b[99999T");
    feed(&mut t, "\x1b[99999X\x1b[99999b\x1b[0;0r\x1b[9;1r\x1b[3;3r\x1b[99999I\x1b[99999Z");
    feed(&mut t, &format!("\x1b[{}m", "1;".repeat(1000)));
    feed(&mut t, &format!("\x1b]0;{}\x07", "x".repeat(100_000)));
    feed(&mut t, "\x1b[38;5m\x1b[38;2;1m\x1b[48m\x1b[58:2m\x1b[;;;m\x1b[:::m");
    feed(&mut t, "\x1bPq\"1;1;99999;99999#999;2;1;2;3!99999~\x1b\\\x18");
    assert_eq!((t.cols, t.rows), (5, 3));
}

#[test]
fn bytes_of_every_kind_do_not_panic() {
    // A small deterministic generator, biased towards bytes that drive the parser.
    let mut t = term(7, 4);
    let mut x: u32 = 12345;
    for _ in 0..300_000 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let v = (x >> 16) as u8;
        let b = match v & 3 {
            0 => 0x1b,
            1 => b"[]P?;0123456789:mHJKLMrhl$\"# q\x07\x18\\"[(v >> 2) as usize % 30],
            _ => v,
        };
        t.feed(&[b]);
    }
    t.resize(3, 2);
    t.feed(b"\x1bc");
    t.resize(9, 9);
}

#[test]
fn resize_keeps_text_and_the_cursor_line() {
    let mut t = term(5, 4);
    feed(&mut t, "1\r\n2\r\n3\r\n4");
    t.resize(5, 2);
    assert_eq!((line(&t, 0).as_str(), line(&t, 1).as_str()), ("3", "4"));
    assert_eq!(t.scrollback_len(), 2);
    assert_eq!(cursor(&t), (1, 1));
    t.resize(5, 4);
    assert_eq!(line(&t, 0), "1");
    assert_eq!(line(&t, 3), "4");
    assert_eq!(cursor(&t), (3, 1));
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn resize_drops_blank_lines_below_the_cursor_first() {
    let mut t = term(5, 4);
    feed(&mut t, "only");
    t.resize(5, 2);
    assert_eq!(line(&t, 0), "only");
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn resize_width_and_margins() {
    let mut t = term(4, 3);
    feed(&mut t, "\x1b[2;3rabcd\x1b[2;2H");
    t.resize(2, 3);
    assert_eq!(line(&t, 0), "ab");
    assert_eq!((t.top, t.bottom), (0, 2));
    t.resize(6, 5);
    feed(&mut t, "\x1b[5;6Hz");
    assert_eq!(line(&t, 4), "     z");
    // A double-width character moves whole, and never leaves half of itself.
    let mut t = term(4, 1);
    feed(&mut t, "a漢");
    t.resize(2, 1);
    assert_eq!(line(&t, 0), "漢");
    assert_eq!(t.scrollback_len(), 1);
    t.scroll_view(1);
    assert_eq!(line(&t, 0), "a");
}

#[test]
fn resize_while_on_the_alternate_screen() {
    let mut t = term(5, 4);
    feed(&mut t, "1\r\n2\r\n3\r\n4\x1b[?1049h\x1b[4;1Halt");
    t.resize(5, 2);
    assert_eq!(line(&t, 1), "alt");
    feed(&mut t, "\x1b[?1049l");
    assert_eq!(line(&t, 1), "4");
    assert_eq!(cursor(&t), (1, 1));
}

#[test]
fn selection_text_across_lines_and_scrollback() {
    let mut t = term(5, 2);
    feed(&mut t, "hello\r\nab  \r\nxy\r\nzz");
    let first = t.first_line();
    t.select_start((first, 1), SelectMode::Char);
    t.select_update((first + 2, 0));
    assert_eq!(t.selection_text().unwrap(), "ello\nab\nx");
}

#[test]
fn selection_joins_wrapped_lines() {
    let mut t = term(3, 3);
    feed(&mut t, "abcdef");
    t.select_all();
    assert_eq!(t.selection_text().unwrap(), "abcdef\n");
}

#[test]
fn word_and_line_selection() {
    let mut t = term(20, 2);
    feed(&mut t, "foo /bar/baz.c  qux");
    let l = t.first_line();
    t.select_start((l, 7), SelectMode::Word);
    assert_eq!(t.selection_text().unwrap(), "/bar/baz.c");
    t.select_start((l, 7), SelectMode::Line);
    assert_eq!(t.selection_text().unwrap(), "foo /bar/baz.c  qux");
}

#[test]
fn selection_follows_text_up_and_clears_when_overwritten() {
    let mut t = term(5, 2);
    feed(&mut t, "keep\r\n");
    let l = t.display_line(0);
    t.select_start((l, 0), SelectMode::Line);
    feed(&mut t, "\r\n\r\n\r\n");
    assert_eq!(t.selection_text().unwrap(), "keep");

    let mut t = term(5, 2);
    feed(&mut t, "keep\x1b[H");
    t.select_all();
    feed(&mut t, "x");
    assert!(t.selection.is_none());
}

#[test]
fn reset_keeps_history_and_clears_state() {
    let mut t = term(5, 2);
    feed(&mut t, "1\r\n2\r\n3\x1b[31m\x1b[?25l\x1b[?2004h\x1bc");
    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(line(&t, 0), "");
    assert!(t.cursor_visible && !t.bracketed_paste);
    feed(&mut t, "a");
    assert_eq!(cell(&t, 0, 0).fg, Color::Default);
}

#[test]
fn soft_reset_keeps_the_screen() {
    let mut t = term(5, 2);
    feed(&mut t, "ab\x1b[?7l\x1b[1;1r\x1b[31m\x1b[!p");
    assert_eq!(line(&t, 0), "ab");
    assert!(t.cur.autowrap);
    assert_eq!(t.cur.pen.fg, Color::Default);
}

#[test]
fn selective_erase_keeps_protected_cells() {
    let mut t = term(5, 1);
    feed(&mut t, "\x1b[1\"qab\x1b[0\"qcd\x1b[?2K");
    assert_eq!(line(&t, 0), "ab");
    feed(&mut t, "\x1b[2K");
    assert_eq!(line(&t, 0), "");
}

#[test]
fn tab_stops_can_be_changed() {
    let mut t = term(20, 1);
    feed(&mut t, "\x1b[3g\x1b[5G\x1bH\x1b[G\tx\x1b[Z");
    assert_eq!(line(&t, 0), "    x");
    assert_eq!(cursor(&t), (0, 4));
}

#[test]
fn repeat_previous_character() {
    let mut t = term(8, 1);
    feed(&mut t, "a\x1b[3b");
    assert_eq!(line(&t, 0), "aaaa");
}

#[test]
fn newline_mode() {
    let mut t = term(5, 2);
    feed(&mut t, "ab\x1b[20h\ncd");
    assert_eq!(line(&t, 1), "cd");
}

#[test]
fn cursor_shapes() {
    let mut t = term(5, 1);
    feed(&mut t, "\x1b[6 q");
    assert_eq!((t.cursor_shape, t.cursor_blink), (CursorShape::Bar, false));
    feed(&mut t, "\x1b[3 q");
    assert_eq!((t.cursor_shape, t.cursor_blink), (CursorShape::Underline, true));
}

#[test]
fn mouse_modes() {
    let mut t = term(5, 1);
    feed(&mut t, "\x1b[?1002h\x1b[?1006h");
    assert_eq!(t.mouse_mode, MouseMode::Drag);
    assert!(t.mouse_sgr);
    feed(&mut t, "\x1b[?1000l");
    assert_eq!(t.mouse_mode, MouseMode::Drag, "turning off another mode changes nothing");
    feed(&mut t, "\x1b[?1002l");
    assert_eq!(t.mouse_mode, MouseMode::Off);
}

fn sixel(t: &mut Terminal, rows_of_6: usize) {
    let body = vec!["#1;2;100;0;0#1!16~"; rows_of_6].join("-");
    feed(t, &format!("\x1bPq{body}\x1b\\"));
}

#[test]
fn sixel_lands_in_cells_and_moves_the_cursor() {
    let mut t = term(10, 6);
    (t.cell_w, t.cell_h) = (8, 12);
    feed(&mut t, "\x1b[2;3H");
    sixel(&mut t, 4); // 16x24 pixels: 2x2 cells.
    assert!(cell(&t, 1, 2).tile().is_some());
    assert!(cell(&t, 2, 3).tile().is_some());
    assert!(cell(&t, 1, 4).tile().is_none());
    assert_eq!(cursor(&t), (3, 2), "the cursor goes below the image, at its left edge");
    let tile = cell(&t, 2, 3).tile().unwrap();
    assert_eq!((tile.col, tile.row), (1, 1));
    assert_eq!((tile.image.width, tile.image.height), (16, 24));
}

#[test]
fn sixel_scrolls_with_text_and_can_be_erased() {
    let mut t = term(10, 4);
    (t.cell_w, t.cell_h) = (8, 12);
    sixel(&mut t, 8); // 4 rows tall, on a 4-row screen: it must scroll to fit.
    assert_eq!(t.scrollback_len(), 1);
    assert!(cell(&t, 2, 0).tile().is_some());
    feed(&mut t, "\x1b[H\x1b[2J");
    assert!(cell(&t, 2, 0).tile().is_none());
    t.scroll_view(1);
    assert!(cell(&t, 0, 0).tile().is_some(), "history keeps the image");
}

#[test]
fn sixel_overwritten_by_text() {
    let mut t = term(10, 4);
    (t.cell_w, t.cell_h) = (8, 12);
    sixel(&mut t, 2);
    feed(&mut t, "\x1b[H\x1b[2CA");
    assert!(cell(&t, 0, 2).tile().is_none());
    assert_eq!(cell(&t, 0, 2).c, 'A');
    assert!(cell(&t, 0, 1).tile().is_some());
}

#[test]
fn sixel_display_mode_keeps_the_cursor_and_clips() {
    let mut t = term(4, 2);
    (t.cell_w, t.cell_h) = (8, 12);
    feed(&mut t, "\x1b[?80h\x1b[2;2H");
    feed(&mut t, "\x1bPq#1;2;0;0;100#1!100~-!100~-!100~-!100~-!100~-!100~\x1b\\");
    assert_eq!(cursor(&t), (1, 1));
    assert!(cell(&t, 0, 3).tile().is_some());
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn sixel_cursor_right_mode() {
    let mut t = term(10, 4);
    (t.cell_w, t.cell_h) = (8, 12);
    feed(&mut t, "\x1b[?8452h");
    sixel(&mut t, 2);
    assert_eq!(cursor(&t), (0, 2));
}

#[test]
fn sixel_query_and_cancel() {
    let mut t = term(10, 4);
    feed(&mut t, "\x1b[?1;1S\x1b[?2;1S");
    assert_eq!(replies(&mut t), "\x1b[?1;0;256S\x1b[?2;0;4096;4096S");
    feed(&mut t, "\x1bPq#1!99~\x18x");
    assert_eq!(cell(&t, 0, 0).c, 'x');
    assert!(cell(&t, 0, 0).tile().is_none());
}

fn screen(t: &Terminal) -> Vec<String> {
    (0..t.rows).map(|y| line(t, y)).collect()
}

/// Everything in the scrollback and on the screen, as the rows now stand.
fn history(t: &mut Terminal) -> Vec<String> {
    let back = t.scrollback_len();
    t.scroll_view(back as isize);
    let mut rows: Vec<String> = (0..back.min(t.rows)).map(|y| line(t, y)).collect();
    let mut left = back.saturating_sub(t.rows);
    while left > 0 {
        t.scroll_view(-(left.min(t.rows) as isize));
        let n = left.min(t.rows);
        rows.extend((0..n).map(|y| line(t, y)));
        left -= n;
    }
    t.view_offset = 0;
    rows.extend(screen(t));
    rows
}

#[test]
fn narrowing_wraps_and_widening_unwraps() {
    let mut t = term(10, 4);
    feed(&mut t, "abcdefghij");
    t.resize(4, 4);
    assert_eq!(screen(&t), ["abcd", "efgh", "ij", ""]);
    assert_eq!(cursor(&t), (2, 2));
    t.resize(10, 4);
    assert_eq!(screen(&t), ["abcdefghij", "", "", ""]);
    assert_eq!(cursor(&t), (0, 9));
    assert!(t.cur.wrap_pending, "the cursor is still waiting at the end of the line");
    feed(&mut t, "k");
    assert_eq!(screen(&t), ["abcdefghij", "k", "", ""]);
}

#[test]
fn hard_newlines_stay() {
    let mut t = term(4, 4);
    feed(&mut t, "abcdef\r\ngh");
    assert_eq!(screen(&t), ["abcd", "ef", "gh", ""]);
    t.resize(8, 4);
    assert_eq!(screen(&t), ["abcdef", "gh", "", ""]);
    assert_eq!(cursor(&t), (1, 2));
}

#[test]
fn text_lost_to_nothing_across_many_widths() {
    let mut t = term(20, 5);
    let text = "the quick brown fox jumps over the lazy dog";
    feed(&mut t, text);
    for cols in [7, 3, 11, 1, 40, 5, 20] {
        t.resize(cols, 5);
    }
    t.select_all();
    assert_eq!(t.selection_text().unwrap().trim_end(), text);
}

#[test]
fn wrapping_grows_into_blank_rows_before_scrolling() {
    let mut t = term(10, 5);
    feed(&mut t, "abcdefghij\r\nk");
    t.resize(5, 5);
    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(screen(&t), ["abcde", "fghij", "k", "", ""]);
}

#[test]
fn overflow_goes_to_the_scrollback_and_comes_back() {
    let mut t = term(10, 2);
    feed(&mut t, "abcdefghij\r\nk");
    t.resize(2, 2);
    assert_eq!(screen(&t), ["ij", "k"]);
    assert_eq!(t.scrollback_len(), 4);
    assert_eq!(cursor(&t), (1, 1));
    t.resize(10, 2);
    assert_eq!(screen(&t), ["abcdefghij", "k"]);
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn scrollback_rewraps_and_stays_bounded() {
    let mut t = Terminal::new(6, 2, 4, Palette::dark().theme_colors());
    feed(&mut t, "abcdefghijkl\r\nm\r\nn\r\no");
    assert_eq!(t.scrollback_len(), 3);
    t.resize(12, 2);
    assert_eq!(history(&mut t), ["abcdefghijkl", "m", "n", "o"]);
    t.resize(2, 2);
    assert_eq!(t.scrollback_len(), 4, "narrowing cannot exceed the configured limit");
    assert_eq!(screen(&t), ["n", "o"]);
    assert_eq!(cursor(&t), (1, 1));
}

#[test]
fn clear_keeps_history_off_the_screen() {
    let mut t = term(10, 3);
    feed(&mut t, "1\r\n2\r\n3\r\n4\r\n5\x1b[2J\x1b[H$ ");
    let back = t.scrollback_len();
    t.resize(8, 3);
    assert_eq!(t.scrollback_len(), back);
    assert_eq!(screen(&t), ["$", "", ""]);
    assert_eq!(cursor(&t), (0, 2), "the space after the prompt survives");
}

#[test]
fn trailing_spaces_before_the_cursor_wrap_too() {
    let mut t = term(10, 3);
    feed(&mut t, "ab   ");
    t.resize(3, 3);
    assert_eq!(cursor(&t), (1, 2));
    feed(&mut t, "c");
    assert_eq!(screen(&t), ["ab", "  c", ""]);
}

#[test]
fn spaces_at_a_wrap_point_survive_the_scrollback() {
    let mut t = term(4, 2);
    feed(&mut t, "abc def ghi jkl\r\nx\r\ny\r\nz");
    assert!(t.scrollback_len() > 0);
    t.resize(20, 2);
    assert_eq!(history(&mut t)[0], "abc def ghi jkl");
}

#[test]
fn wide_characters_stay_whole_and_padding_is_not_text() {
    let mut t = term(8, 3);
    feed(&mut t, "a漢字b漢");
    t.resize(4, 3);
    assert_eq!(screen(&t), ["a漢", "字b", "漢"]);
    assert!(cell(&t, 0, 3).is_wrap_pad());
    t.select_all();
    assert_eq!(t.selection_text().unwrap().trim_end(), "a漢字b漢", "copying skips the padding");
    t.resize(8, 3);
    assert_eq!(screen(&t), ["a漢字b漢", "", ""]);
    assert_eq!(cursor(&t), (0, 7));
}

#[test]
fn padding_from_a_wide_character_that_did_not_fit_goes_on_rejoining() {
    let mut t = term(3, 2);
    feed(&mut t, "ab漢");
    t.resize(6, 2);
    assert_eq!(screen(&t), ["ab漢", ""]);
}

#[test]
fn wide_character_on_a_single_column_is_dropped_safely() {
    let mut t = term(4, 2);
    feed(&mut t, "a漢b");
    t.resize(1, 2);
    t.resize(4, 2);
    assert_eq!(t.cols, 4);
}

#[test]
fn attributes_and_combining_marks_move_with_their_text() {
    let mut t = term(6, 3);
    feed(&mut t, "abcd\x1b[31me\u{301}f");
    t.resize(4, 3);
    assert_eq!(line(&t, 1), "e\u{301}f");
    assert_eq!(cell(&t, 1, 0).fg, Color::Indexed(1));
}

#[test]
fn alternate_screen_is_not_rewrapped_but_the_primary_is() {
    let mut t = term(10, 3);
    feed(&mut t, "abcdefghij\x1b[?1049h\x1b[Hfull screen");
    t.resize(5, 3);
    assert_eq!(line(&t, 0), "full", "cut, not wrapped");
    feed(&mut t, "\x1b[?1049l");
    assert_eq!(screen(&t), ["abcde", "fghij", ""]);
    assert_eq!(cursor(&t), (1, 4));
    feed(&mut t, "k");
    assert_eq!(line(&t, 2), "k");
}

#[test]
fn images_keep_their_rows() {
    let mut t = term(10, 4);
    (t.cell_w, t.cell_h) = (8, 12);
    sixel(&mut t, 2);
    feed(&mut t, "abcdefghij");
    t.resize(5, 4);
    assert!(cell(&t, 0, 0).tile().is_some());
    assert!(cell(&t, 0, 1).tile().is_some());
    assert!(cell(&t, 1, 0).tile().is_none());
    assert_eq!(line(&t, 1), "abcde");
    assert_eq!(line(&t, 2), "fghij");
}

#[test]
fn saved_cursor_moves_with_its_text() {
    let mut t = term(10, 4);
    feed(&mut t, "abcdefgh\x1b7\x1b[4;1H");
    t.resize(4, 4);
    feed(&mut t, "\x1b8Z");
    assert_eq!(screen(&t), ["efgh", "Z", "", ""], "it continues right after the text");
}

#[test]
fn scrolled_back_view_stays_on_its_text() {
    let mut t = term(4, 2);
    for i in 0..9 {
        feed(&mut t, &format!("{i}\r\n"));
    }
    t.scroll_view(3);
    let before = line(&t, 0);
    t.resize(6, 2);
    assert_eq!(line(&t, 0), before);
    assert!(t.view_offset > 0);
}

#[test]
fn height_and_width_change_together() {
    let mut t = term(10, 2);
    feed(&mut t, "abcdefghij");
    t.resize(5, 6);
    assert_eq!(screen(&t), ["abcde", "fghij", "", "", "", ""]);
    feed(&mut t, "\x1b[6;5Hz");
    assert_eq!(line(&t, 5), "    z");
}

#[test]
fn resizing_output_at_every_stage_keeps_the_screen_consistent() {
    let mut t = term(7, 4);
    let mut x: u32 = 99;
    let mut next = |n: u32| {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        (x >> 16) % n
    };
    let pieces = ["word ", "漢字", "\r\n", "\x1b[2J", "\x1b[H", "\x1b[?1049h", "\x1b[?1049l", "\x1b7", "\x1b8",
        "e\u{301}", "\x1b[K", "\x1b[5;2r", "\x1b[r", "\x1b[3L", "\x1b[2M", "\x1bc", "\x1b[3S", "aaaaaaaaaaaaaaaaaaaa"];
    for _ in 0..20_000 {
        feed(&mut t, pieces[next(pieces.len() as u32) as usize]);
        if next(4) == 0 {
            t.resize(1 + next(30) as usize, 1 + next(12) as usize);
            t.scroll_view(next(5) as isize);
        }
        assert_eq!(t.primary.len(), t.rows);
        assert_eq!(t.alternate.len(), t.rows);
        assert!(t.primary.iter().chain(t.alternate.iter()).all(|r| r.cells.len() == t.cols));
        assert!(t.cur.row < t.rows && t.cur.col < t.cols);
        assert!(t.scrollback.len() <= t.max_scrollback);
        assert!(t.pushed >= t.scrollback.len() as u64);
        (0..t.rows).for_each(|y| assert!(t.display_row(y).cells.len() <= t.cols.max(64)));
    }
}

#[test]
#[ignore = "timing report; run with --ignored --nocapture"]
fn reflow_speed() {
    for lines in [10_000, 100_000] {
        let mut t = Terminal::new(100, 40, lines, Palette::dark().theme_colors());
        let text = "x".repeat(250) + "\r\n";
        for _ in 0..lines {
            feed(&mut t, &text);
        }
        let start = std::time::Instant::now();
        t.resize(83, 40);
        println!("{lines} long lines: {:?}", start.elapsed());
    }
}
