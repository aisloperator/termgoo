//! The terminal widget: draws the screen, and connects the keyboard, the mouse,
//! the clipboard and the program to the `Terminal`.

use crate::config::{Palette, Rgb};
use crate::input::{self, KeyModes, MouseAction};
use crate::pty::Pty;
use crate::term::cell::{self, Cell as TermCell, Color, flag};
use crate::term::{CursorShape, MouseMode, Pos, SelectMode, Terminal};
use gtk::gdk::{self, Key, ModifierType as M};
use gtk::glib::{self, ControlFlow, Propagation, clone};
use gtk::graphene::{Point, Rect};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, pango};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::time::{Duration, Instant};

/// Blank margin around the character grid.
const PADDING: i32 = 2;
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Longest time to spend on program output before letting the window respond.
const READ_BUDGET: Duration = Duration::from_millis(30);
const LAYOUT_CACHE_LIMIT: usize = 16384;
const WHEEL_LINES: f64 = 3.0;
const BOLD: u8 = 1;
const ITALIC: u8 = 2;

fn rgba((r, g, b): Rgb) -> gdk::RGBA {
    gdk::RGBA::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

/// Box drawing and block elements, which are always drawn one cell at a time.
fn is_line_art(text: &str) -> bool {
    text.chars().next().is_some_and(|c| ('\u{2500}'..='\u{259f}').contains(&c))
}

/// Metrics and cached text layouts for the current font.
struct Font {
    descriptions: [pango::FontDescription; 4],
    cell_w: i32,
    cell_h: i32,
    baseline: i32,
    /// Fits the font's real advance to the whole-pixel cell width.
    letter_spacing: i32,
    layouts: RefCell<HashMap<(u8, String), pango::Layout>>,
}

impl Font {
    fn new(widget: &impl IsA<gtk::Widget>, name: &str) -> Font {
        let base = pango::FontDescription::from_string(name);
        let descriptions = [0, BOLD, ITALIC, BOLD | ITALIC].map(|style| {
            let mut d = base.clone();
            if style & BOLD != 0 {
                d.set_weight(pango::Weight::Bold);
            }
            if style & ITALIC != 0 {
                d.set_style(pango::Style::Italic);
            }
            d
        });
        let layout = widget.as_ref().create_pango_layout(Some("M"));
        layout.set_font_description(Some(&descriptions[0]));
        let (_, logical) = layout.extents();
        let cell_w = pango::units_to_double(logical.width()).ceil().max(1.0) as i32;
        let cell_h = pango::units_to_double(logical.height()).ceil().max(1.0) as i32;
        Font {
            cell_w,
            cell_h,
            baseline: layout.baseline() / pango::SCALE,
            letter_spacing: cell_w * pango::SCALE - logical.width(),
            descriptions,
            layouts: RefCell::new(HashMap::new()),
        }
    }

    /// Draw on the grid's baseline: a fallback font's own may be lower or higher.
    fn draw(&self, s: &gtk::Snapshot, layout: &pango::Layout, text: &str, x: f32, top: f32, color: Rgb) {
        s.save();
        if is_line_art(text) {
            // Line art has to touch its neighbours, so it fills the cell exactly.
            let (_, logical) = layout.extents();
            s.translate(&Point::new(x, top));
            s.scale(
                self.cell_w as f32 * pango::SCALE as f32 / logical.width().max(1) as f32,
                self.cell_h as f32 * pango::SCALE as f32 / logical.height().max(1) as f32,
            );
        } else {
            let shift = (self.baseline * pango::SCALE - layout.baseline()) as f32 / pango::SCALE as f32;
            s.translate(&Point::new(x, top + shift));
        }
        s.append_layout(layout, &rgba(color));
        s.restore();
    }

    fn layout(&self, widget: &gtk::Widget, style: u8, text: &str) -> pango::Layout {
        let mut cache = self.layouts.borrow_mut();
        // The lookup allocates only for a text not seen on an earlier frame.
        let key = (style, text.to_string());
        if let Some(l) = cache.get(&key) {
            return l.clone();
        }
        if cache.len() >= LAYOUT_CACHE_LIMIT {
            cache.clear();
        }
        let layout = widget.create_pango_layout(Some(text));
        layout.set_font_description(Some(&self.descriptions[style as usize]));
        let attrs = pango::AttrList::new();
        if !is_line_art(text) {
            attrs.insert(pango::AttrInt::new_letter_spacing(self.letter_spacing));
        }
        attrs.insert(pango::AttrFontFeatures::new("liga 0, clig 0, calt 0"));
        layout.set_attributes(Some(&attrs));
        cache.insert(key, layout.clone());
        layout
    }
}

/// How a cell looks once colours, inversion and selection are worked out.
struct Look {
    fg: Rgb,
    /// None for the default background, which is already painted.
    bg: Option<Rgb>,
}

type Callback = Box<dyn Fn()>;
type TitleCallback = Box<dyn Fn(&str)>;

pub struct Inner {
    pub term: RefCell<Terminal>,
    pub pty: RefCell<Pty>,
    palette: RefCell<Palette>,
    font: RefCell<Font>,
    natural: (usize, usize),
    hold: bool,
    im: gtk::IMMulticontext,
    pub adjustment: gtk::Adjustment,
    syncing: Cell<bool>,
    popover: OnceCell<gtk::PopoverMenu>,
    on_title: RefCell<Option<TitleCallback>>,
    on_exit: RefCell<Option<Callback>>,
    exited: Cell<bool>,
    write_source: RefCell<Option<glib::SourceId>>,

    focused: Cell<bool>,
    blink_on: Cell<bool>,
    blinking_cells: Cell<bool>,
    selecting: Cell<bool>,
    mouse_button: Cell<Option<u32>>,
    last_mouse_cell: Cell<(usize, usize)>,
    /// Where the pointer is. Scroll events do not carry a position of their own.
    pointer_cell: Cell<(usize, usize)>,
    last_click: Cell<(u32, Pos, u32)>,
    scroll_remainder: Cell<f64>,
    menu_prepare: RefCell<Option<Callback>>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TermView {
        pub inner: OnceCell<Inner>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TermView {
        const NAME: &'static str = "TermGooView";
        type Type = super::TermView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TermView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TermView {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let Some(inner) = self.inner.get() else { return (0, 0, -1, -1) };
            let font = inner.font.borrow();
            let (cols, rows) = inner.natural;
            let (min, natural) = match orientation {
                gtk::Orientation::Horizontal => (2 * font.cell_w, cols as i32 * font.cell_w),
                _ => (font.cell_h, rows as i32 * font.cell_h),
            };
            (min + 2 * PADDING, natural + 2 * PADDING, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            if let Some(popover) = self.inner.get().and_then(|i| i.popover.get()) {
                popover.present();
            }
            self.obj().apply_size(width, height);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().render(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct TermView(ObjectSubclass<imp::TermView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub struct Settings<'a> {
    pub command: Option<&'a [String]>,
    pub term: &'a str,
    pub font: &'a str,
    pub size: (usize, usize),
    pub scrollback: usize,
    pub palette: Palette,
    pub hold: bool,
}

impl TermView {
    pub fn new(s: Settings) -> std::io::Result<TermView> {
        let obj: TermView = glib::Object::new();
        let font = Font::new(&obj, s.font);
        let cell = (font.cell_w as u32, font.cell_h as u32);
        let pty = Pty::spawn(s.command, s.term, s.size, cell)?;
        let mut term = Terminal::new(s.size.0, s.size.1, s.scrollback, s.palette.theme_colors());
        (term.cell_w, term.cell_h) = cell;
        let inner = Inner {
            term: RefCell::new(term),
            pty: RefCell::new(pty),
            palette: RefCell::new(s.palette),
            font: RefCell::new(font),
            natural: s.size,
            hold: s.hold,
            im: gtk::IMMulticontext::new(),
            adjustment: gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
            syncing: Cell::new(false),
            popover: OnceCell::new(),
            on_title: RefCell::new(None),
            on_exit: RefCell::new(None),
            exited: Cell::new(false),
            write_source: RefCell::new(None),
            focused: Cell::new(false),
            blink_on: Cell::new(true),
            blinking_cells: Cell::new(false),
            selecting: Cell::new(false),
            mouse_button: Cell::new(None),
            last_mouse_cell: Cell::new((usize::MAX, usize::MAX)),
            pointer_cell: Cell::new((0, 0)),
            last_click: Cell::new((0, (0, 0), 0)),
            scroll_remainder: Cell::new(0.0),
            menu_prepare: RefCell::new(None),
        };
        obj.imp().inner.set(inner).ok().expect("set once");
        obj.set_focusable(true);
        obj.set_can_focus(true);
        obj.set_hexpand(true);
        obj.set_vexpand(true);
        obj.set_cursor_from_name(Some("text"));
        obj.setup_keyboard();
        obj.setup_mouse();
        obj.setup_program();
        obj.setup_blink();
        obj.sync_adjustment();
        Ok(obj)
    }

    fn inner(&self) -> &Inner {
        self.imp().inner.get().expect("built by TermView::new")
    }

    pub fn adjustment(&self) -> gtk::Adjustment {
        self.inner().adjustment.clone()
    }

    pub fn connect_title(&self, f: impl Fn(&str) + 'static) {
        *self.inner().on_title.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_exit(&self, f: impl Fn() + 'static) {
        *self.inner().on_exit.borrow_mut() = Some(Box::new(f));
    }

    /// Attach the context menu. `prepare` runs before each time it is shown.
    pub fn set_menu(&self, model: &gio::Menu, prepare: impl Fn() + 'static) {
        let popover = gtk::PopoverMenu::from_model(Some(model));
        popover.set_parent(self);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.connect_closed(clone!(#[weak(rename_to = view)] self, move |_| {
            view.grab_focus();
        }));
        self.inner().popover.set(popover).ok();
        *self.inner().menu_prepare.borrow_mut() = Some(Box::new(prepare));
    }

    fn show_menu(&self, x: f64, y: f64) {
        let inner = self.inner();
        if let Some(prepare) = &*inner.menu_prepare.borrow() {
            prepare();
        }
        if let Some(popover) = inner.popover.get() {
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    }

    // ---- Settings that can change while running ---------------------------------

    pub fn set_palette(&self, palette: Palette) {
        let inner = self.inner();
        inner.term.borrow_mut().theme_colors = palette.theme_colors();
        *inner.palette.borrow_mut() = palette;
        self.queue_draw();
    }

    pub fn set_font(&self, name: &str) {
        let inner = self.inner();
        let font = Font::new(self, name);
        {
            let mut term = inner.term.borrow_mut();
            term.cell_w = font.cell_w as u32;
            term.cell_h = font.cell_h as u32;
        }
        *inner.font.borrow_mut() = font;
        self.queue_resize();
        self.queue_draw();
    }

    pub fn set_scrollback(&self, lines: usize) {
        self.inner().term.borrow_mut().set_max_scrollback(lines);
        self.sync_adjustment();
        self.queue_draw();
    }

    fn apply_size(&self, width: i32, height: i32) {
        let inner = self.inner();
        let (cw, ch) = {
            let f = inner.font.borrow();
            (f.cell_w, f.cell_h)
        };
        let cols = ((width - 2 * PADDING) / cw).max(1) as usize;
        let rows = ((height - 2 * PADDING) / ch).max(1) as usize;
        {
            let mut term = inner.term.borrow_mut();
            if (term.cols, term.rows) == (cols, rows) {
                return;
            }
            term.resize(cols, rows);
            inner.pty.borrow().resize(cols, rows, (term.cell_w, term.cell_h));
        }
        self.sync_adjustment();
    }

    // ---- Scrolling --------------------------------------------------------------

    fn sync_adjustment(&self) {
        let inner = self.inner();
        let term = inner.term.borrow();
        let (back, rows) = (term.scrollback_len() as f64, term.rows as f64);
        let value = back - term.view_offset as f64;
        drop(term);
        inner.syncing.set(true);
        inner.adjustment.configure(value, 0.0, back + rows, 1.0, rows, rows);
        inner.syncing.set(false);
    }

    pub fn setup_scrollbar_link(&self) {
        let view = self.clone();
        self.inner().adjustment.connect_value_changed(move |adj| {
            let inner = view.inner();
            if inner.syncing.get() {
                return;
            }
            let mut term = inner.term.borrow_mut();
            term.view_offset = (term.scrollback_len() as f64 - adj.value()).round().max(0.0) as usize;
            drop(term);
            view.queue_draw();
        });
    }

    fn scroll_view(&self, delta: isize) {
        self.inner().term.borrow_mut().scroll_view(delta);
        self.sync_adjustment();
        self.queue_draw();
    }

    fn scroll_to_bottom(&self) {
        let offset = self.inner().term.borrow().view_offset;
        if offset != 0 {
            self.scroll_view(-(offset as isize));
        }
    }

    // ---- The program ------------------------------------------------------------

    fn setup_program(&self) {
        let fd = self.inner().pty.borrow().fd();
        // Idle priority: input and drawing come first, however much the program writes.
        glib_unix::unix_fd_add_local_full(
            fd,
            glib::Priority::DEFAULT_IDLE,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            clone!(#[weak(rename_to = view)] self, #[upgrade_or] ControlFlow::Break, move |_, _| {
                view.read_program()
            }),
        );
        let pid = glib::Pid(self.inner().pty.borrow().pid);
        glib::child_watch_add_local(pid, clone!(#[weak(rename_to = view)] self, move |_, _| {
            view.program_exited();
        }));
    }

    fn read_program(&self) -> ControlFlow {
        let inner = self.inner();
        let mut buf = [0u8; 65536];
        let start = Instant::now();
        let mut flow = ControlFlow::Continue;
        let mut got = false;
        while start.elapsed() < READ_BUDGET {
            match inner.pty.borrow().read(&mut buf) {
                Ok(0) => {
                    flow = ControlFlow::Break;
                    break;
                }
                Ok(n) => {
                    inner.term.borrow_mut().feed(&buf[..n]);
                    got = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                // Linux reports EIO once the last process using the terminal has gone.
                Err(_) => {
                    flow = ControlFlow::Break;
                    break;
                }
            }
        }
        if got {
            self.after_feed();
        }
        flow
    }

    fn after_feed(&self) {
        let inner = self.inner();
        let (replies, title, bell) = {
            let mut term = inner.term.borrow_mut();
            (std::mem::take(&mut term.replies), term.new_title.take(), std::mem::take(&mut term.bell))
        };
        if !replies.is_empty() {
            self.send(&replies);
        }
        if let (Some(t), Some(f)) = (title, &*inner.on_title.borrow()) {
            f(&t);
        }
        if bell {
            WidgetExt::display(self).beep();
        }
        self.sync_adjustment();
        self.queue_draw();
    }

    fn program_exited(&self) {
        let inner = self.inner();
        if inner.exited.replace(true) || inner.hold {
            return;
        }
        if let Some(f) = &*inner.on_exit.borrow() {
            f();
        }
    }

    /// Send bytes to the program, waiting for room in the background if needed.
    fn send(&self, bytes: &[u8]) {
        let inner = self.inner();
        let pending = {
            let mut pty = inner.pty.borrow_mut();
            pty.write(bytes);
            pty.has_pending()
        };
        if pending && inner.write_source.borrow().is_none() {
            let fd = inner.pty.borrow().fd();
            let id = glib_unix::unix_fd_add_local(fd, glib::IOCondition::OUT, clone!(
                #[weak(rename_to = view)] self,
                #[upgrade_or] ControlFlow::Break,
                move |_, _| {
                    let inner = view.inner();
                    let mut pty = inner.pty.borrow_mut();
                    pty.flush();
                    if pty.has_pending() {
                        return ControlFlow::Continue;
                    }
                    inner.write_source.borrow_mut().take();
                    ControlFlow::Break
                }
            ));
            *inner.write_source.borrow_mut() = Some(id);
        }
    }

    // ---- Keyboard ---------------------------------------------------------------

    fn setup_keyboard(&self) {
        let inner = self.inner();
        let keys = gtk::EventControllerKey::new();
        keys.set_im_context(Some(&inner.im));
        inner.im.set_use_preedit(false);
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = view)] self,
            #[upgrade_or] Propagation::Proceed,
            move |_, key, _, state| view.key_pressed(key, state)
        ));
        self.add_controller(keys);

        inner.im.connect_commit(clone!(#[weak(rename_to = view)] self, move |_, text| {
            view.scroll_to_bottom();
            view.send(text.as_bytes());
        }));

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(clone!(#[weak(rename_to = view)] self, move |_| view.focus_changed(true)));
        focus.connect_leave(clone!(#[weak(rename_to = view)] self, move |_| view.focus_changed(false)));
        self.add_controller(focus);

        self.connect_realize(|view| view.inner().im.set_client_widget(Some(view)));
        self.connect_unrealize(|view| view.inner().im.set_client_widget(None::<&gtk::Widget>));
    }

    fn focus_changed(&self, focused: bool) {
        let inner = self.inner();
        inner.focused.set(focused);
        if focused {
            inner.im.focus_in();
        } else {
            inner.im.focus_out();
        }
        if inner.term.borrow().focus_reporting {
            self.send(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        self.queue_draw();
    }

    fn key_pressed(&self, key: Key, state: M) -> Propagation {
        let mods = state & (M::SHIFT_MASK | M::CONTROL_MASK | M::ALT_MASK | M::SUPER_MASK | M::META_MASK);
        let ctrl_shift = M::CONTROL_MASK | M::SHIFT_MASK;
        if mods == ctrl_shift {
            match key.to_upper() {
                Key::C => {
                    self.copy();
                    return Propagation::Stop;
                }
                Key::V => {
                    self.paste();
                    return Propagation::Stop;
                }
                _ => {}
            }
        }
        if mods == M::SHIFT_MASK {
            let (rows, back) = {
                let term = self.inner().term.borrow();
                (term.rows as isize, term.scrollback_len() as isize)
            };
            match key {
                Key::Page_Up => self.scroll_view(rows - 1),
                Key::Page_Down => self.scroll_view(1 - rows),
                Key::Home => self.scroll_view(back),
                Key::End => self.scroll_to_bottom(),
                Key::Insert => {
                    self.paste_from(&WidgetExt::primary_clipboard(self));
                }
                _ => return self.send_key(key, mods),
            }
            return Propagation::Stop;
        }
        self.send_key(key, mods)
    }

    fn send_key(&self, key: Key, mods: M) -> Propagation {
        let modes = {
            let term = self.inner().term.borrow();
            KeyModes { app_cursor: term.app_cursor, app_keypad: term.app_keypad, newline: term.newline_mode }
        };
        match input::encode_key(key, mods, modes) {
            Some(bytes) => {
                self.scroll_to_bottom();
                self.send(&bytes);
                Propagation::Stop
            }
            None => Propagation::Proceed,
        }
    }

    // ---- Clipboard --------------------------------------------------------------

    pub fn has_selection(&self) -> bool {
        self.inner().term.borrow().selection.is_some()
    }

    pub fn copy(&self) {
        if let Some(text) = self.inner().term.borrow().selection_text() {
            WidgetExt::clipboard(self).set_text(&text);
        }
    }

    pub fn paste(&self) {
        self.paste_from(&WidgetExt::clipboard(self));
    }

    fn paste_from(&self, clipboard: &gdk::Clipboard) {
        clipboard.read_text_async(None::<&gio::Cancellable>, clone!(
            #[weak(rename_to = view)] self,
            move |result| {
                if let Ok(Some(text)) = result {
                    view.paste_text(&text);
                }
            }
        ));
    }

    fn paste_text(&self, text: &str) {
        let bracketed = self.inner().term.borrow().bracketed_paste;
        self.scroll_to_bottom();
        self.send(&input::paste_bytes(text, bracketed));
    }

    pub fn select_all(&self) {
        self.inner().term.borrow_mut().select_all();
        self.publish_selection();
        self.queue_draw();
    }

    /// The X11 tradition: what is selected can be pasted with the middle button.
    fn publish_selection(&self) {
        if let Some(text) = self.inner().term.borrow().selection_text() {
            WidgetExt::primary_clipboard(self).set_text(&text);
        }
    }

    // ---- Commands from the menu -------------------------------------------------

    pub fn clear_scrollback(&self) {
        self.inner().term.borrow_mut().clear_scrollback();
        self.sync_adjustment();
        self.queue_draw();
    }

    pub fn reset(&self) {
        self.inner().term.borrow_mut().reset();
        self.sync_adjustment();
        self.queue_draw();
    }

    // ---- Mouse ------------------------------------------------------------------

    fn setup_mouse(&self) {
        // One controller for presses, releases and motion gives exact control over
        // drags, which the higher-level gestures would interpret for themselves.
        let legacy = gtk::EventControllerLegacy::new();
        legacy.connect_event(clone!(
            #[weak(rename_to = view)] self,
            #[upgrade_or] Propagation::Proceed,
            move |_, event| view.pointer_event(event)
        ));
        self.add_controller(legacy);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(clone!(
            #[weak(rename_to = view)] self,
            #[upgrade_or] Propagation::Proceed,
            move |controller, _, dy| {
                view.scrolled(controller, dy);
                Propagation::Stop
            }
        ));
        self.add_controller(scroll);
    }

    /// An event's position, in this widget's coordinates.
    fn event_point(&self, event: &gdk::Event) -> Option<(f64, f64)> {
        let (x, y) = event.position()?;
        let native = self.native()?;
        let (sx, sy) = native.surface_transform();
        let p = native.compute_point(self, &Point::new((x - sx) as f32, (y - sy) as f32))?;
        Some((p.x() as f64, p.y() as f64))
    }

    /// The cell under a point, clamped to the screen.
    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let inner = self.inner();
        let font = inner.font.borrow();
        let term = inner.term.borrow();
        let col = ((x - PADDING as f64) / font.cell_w as f64).floor().max(0.0) as usize;
        let row = ((y - PADDING as f64) / font.cell_h as f64).floor().max(0.0) as usize;
        (col.min(term.cols - 1), row.min(term.rows - 1))
    }

    fn pointer_event(&self, event: &gdk::Event) -> Propagation {
        match event.event_type() {
            gdk::EventType::ButtonPress => self.button_press(event),
            gdk::EventType::ButtonRelease => self.button_release(event),
            gdk::EventType::MotionNotify => self.pointer_motion(event),
            _ => {}
        }
        Propagation::Proceed
    }

    /// Mouse events belong to the program if it asked for them. Shift takes them
    /// back, so that selecting text and the menu stay available in every program.
    fn reporting(&self, mods: M) -> bool {
        self.inner().term.borrow().mouse_mode != MouseMode::Off && !mods.contains(M::SHIFT_MASK)
    }

    fn report_mouse(&self, action: MouseAction, button: u32, mods: M, cell: (usize, usize)) {
        let sgr = {
            let term = self.inner().term.borrow();
            if term.mouse_mode == MouseMode::X10 && action != MouseAction::Press {
                return;
            }
            term.mouse_sgr
        };
        self.send(&input::encode_mouse(action, button, mods, cell.0, cell.1, sgr));
    }

    fn button_press(&self, event: &gdk::Event) {
        let Some(button_event) = event.downcast_ref::<gdk::ButtonEvent>() else { return };
        let Some((x, y)) = self.event_point(event) else { return };
        let (button, mods) = (button_event.button(), event.modifier_state());
        let inner = self.inner();
        self.grab_focus();

        let cell = self.cell_at(x, y);
        inner.pointer_cell.set(cell);
        if self.reporting(mods) {
            if let Some(code) = button_code(button) {
                inner.mouse_button.set(Some(code));
                self.report_mouse(MouseAction::Press, code, mods, cell);
            }
            return;
        }
        match button {
            gdk::BUTTON_PRIMARY => {
                let pos = self.position_at(cell);
                let mode = self.click_mode(pos, event.time());
                inner.term.borrow_mut().select_start(pos, mode);
                inner.selecting.set(true);
                self.queue_draw();
            }
            gdk::BUTTON_MIDDLE => self.paste_from(&WidgetExt::primary_clipboard(self)),
            gdk::BUTTON_SECONDARY => self.show_menu(x, y),
            _ => {}
        }
    }

    fn position_at(&self, cell: (usize, usize)) -> Pos {
        (self.inner().term.borrow().display_line(cell.1), cell.0)
    }

    /// One, two and three clicks select by character, word and line.
    fn click_mode(&self, pos: Pos, time: u32) -> SelectMode {
        let inner = self.inner();
        let limit = WidgetExt::settings(self).gtk_double_click_time().max(0) as u32;
        let (last_time, last_pos, count) = inner.last_click.get();
        let same = pos == last_pos && time.wrapping_sub(last_time) <= limit;
        let count = if same { count % 3 + 1 } else { 1 };
        inner.last_click.set((time, pos, count));
        match count {
            1 => SelectMode::Char,
            2 => SelectMode::Word,
            _ => SelectMode::Line,
        }
    }

    fn button_release(&self, event: &gdk::Event) {
        let Some((x, y)) = self.event_point(event) else { return };
        let inner = self.inner();
        if inner.selecting.replace(false) {
            let mut term = inner.term.borrow_mut();
            // A click without a drag leaves nothing selected.
            if term.selection.is_some_and(|s| s.mode == SelectMode::Char && s.start == s.end) {
                term.selection = None;
            }
            drop(term);
            self.publish_selection();
            self.queue_draw();
        } else if let Some(code) = inner.mouse_button.take() {
            self.report_mouse(MouseAction::Release, code, event.modifier_state(), self.cell_at(x, y));
        }
    }

    fn pointer_motion(&self, event: &gdk::Event) {
        let Some((x, y)) = self.event_point(event) else { return };
        let inner = self.inner();
        let mods = event.modifier_state();
        inner.pointer_cell.set(self.cell_at(x, y));
        if inner.selecting.get() {
            // Dragging past the top or bottom scrolls the history along.
            if y < 0.0 {
                self.scroll_view(1);
            } else if y >= WidgetExt::height(self) as f64 {
                self.scroll_view(-1);
            }
            let pos = self.position_at(self.cell_at(x, y));
            inner.term.borrow_mut().select_update(pos);
            self.queue_draw();
            return;
        }
        let mode = inner.term.borrow().mouse_mode;
        let held = inner.mouse_button.get();
        let wanted = match mode {
            MouseMode::Any => self.reporting(mods),
            MouseMode::Drag => held.is_some() && self.reporting(mods),
            _ => false,
        };
        let cell = self.cell_at(x, y);
        if wanted && inner.last_mouse_cell.replace(cell) != cell {
            self.report_mouse(MouseAction::Motion, held.unwrap_or(3), mods, cell);
        }
    }

    fn scrolled(&self, controller: &gtk::EventControllerScroll, dy: f64) {
        let inner = self.inner();
        let lines_per_unit = match controller.unit() {
            gdk::ScrollUnit::Surface => 1.0 / inner.font.borrow().cell_h as f64,
            _ => WHEEL_LINES,
        };
        let total = inner.scroll_remainder.get() + dy * lines_per_unit;
        let lines = total.trunc();
        inner.scroll_remainder.set(total - lines);
        let lines = lines as isize;
        if lines == 0 {
            return;
        }
        let mods = controller.current_event_state();
        let (using_alt, alt_scroll, app_cursor) = {
            let t = inner.term.borrow();
            (t.using_alt, t.alternate_scroll, t.app_cursor)
        };
        if self.reporting(mods) {
            // One report per wheel notch, not per line of scrolling.
            let code = if lines < 0 { 64 } else { 65 };
            let notches = ((lines.abs() as f64 / WHEEL_LINES).round() as usize).max(1);
            for _ in 0..notches {
                self.report_mouse(MouseAction::Press, code, mods, inner.pointer_cell.get());
            }
        } else if using_alt {
            if alt_scroll {
                // Full-screen programs without mouse support scroll with arrow keys.
                let key = input::encode_key(if lines < 0 { Key::Up } else { Key::Down }, M::empty(), KeyModes {
                    app_cursor,
                    ..KeyModes::default()
                });
                if let Some(bytes) = key {
                    (0..lines.abs()).for_each(|_| self.send(&bytes));
                }
            }
        } else {
            self.scroll_view(-lines);
        }
    }

    // ---- Blinking ---------------------------------------------------------------

    fn setup_blink(&self) {
        glib::timeout_add_local(BLINK_INTERVAL, clone!(
            #[weak(rename_to = view)] self,
            #[upgrade_or] ControlFlow::Break,
            move || {
                let inner = view.inner();
                let cursor = inner.term.borrow().cursor_blink && inner.focused.get();
                if cursor || inner.blinking_cells.get() {
                    inner.blink_on.set(!inner.blink_on.get());
                    view.queue_draw();
                } else {
                    inner.blink_on.set(true);
                }
                ControlFlow::Continue
            }
        ));
    }

    // ---- Drawing ----------------------------------------------------------------

    fn default_colors(&self, term: &Terminal, palette: &Palette) -> (Rgb, Rgb) {
        let fg = term.fg_override.unwrap_or(palette.foreground);
        let bg = term.bg_override.unwrap_or(palette.background);
        if term.reverse_video { (bg, fg) } else { (fg, bg) }
    }

    fn look(&self, term: &Terminal, cell: &TermCell, defaults: (Rgb, Rgb), selection: Option<Rgb>) -> Look {
        let resolve = |c: Color| match c {
            Color::Default => None,
            Color::Indexed(i) => Some(term.palette_color(i)),
            Color::Rgb(r, g, b) => Some((r, g, b)),
        };
        let mut fg = resolve(cell.fg).unwrap_or(defaults.0);
        let mut bg = resolve(cell.bg);
        if cell.flags & flag::INVERSE != 0 {
            let old_bg = bg.unwrap_or(defaults.1);
            bg = Some(fg);
            fg = old_bg;
        }
        if cell.flags & flag::DIM != 0 {
            let b = bg.unwrap_or(defaults.1);
            let mix = |f: u8, b: u8| ((f as u16 * 2 + b as u16) / 3) as u8;
            fg = (mix(fg.0, b.0), mix(fg.1, b.1), mix(fg.2, b.2));
        }
        if cell.flags & flag::HIDDEN != 0 {
            fg = bg.unwrap_or(defaults.1);
        }
        Look { fg, bg: selection.or(bg) }
    }

    fn render(&self, s: &gtk::Snapshot) {
        let inner = self.inner();
        let term = inner.term.borrow();
        let palette = inner.palette.borrow();
        let font = inner.font.borrow();
        let defaults = self.default_colors(&term, &palette);
        let (cw, ch) = (font.cell_w as f32, font.cell_h as f32);
        let pad = PADDING as f32;
        let widget: &gtk::Widget = self.upcast_ref();
        let blink_on = inner.blink_on.get();
        let mut blinking = false;

        s.append_color(&rgba(defaults.1), &Rect::new(0.0, 0.0, WidgetExt::width(self) as f32, WidgetExt::height(self) as f32));

        for y in 0..term.rows {
            let row = term.display_row(y);
            let line = term.display_line(y);
            let top = pad + y as f32 * ch;
            let cells = &row.cells[..row.cells.len().min(term.cols)];
            let selected = |x: usize| {
                term.selection.filter(|sel| sel.contains((line, x))).map(|_| palette.selection)
            };

            // Backgrounds, merged into spans.
            let mut x = 0;
            while x < term.cols {
                let start = x;
                let bg = match cells.get(x) {
                    Some(c) => self.look(&term, c, defaults, selected(x)).bg,
                    None => selected(x),
                };
                x += 1;
                while x < term.cols {
                    let next = match cells.get(x) {
                        Some(c) => self.look(&term, c, defaults, selected(x)).bg,
                        None => selected(x),
                    };
                    if next != bg {
                        break;
                    }
                    x += 1;
                }
                if let Some(bg) = bg {
                    let left = pad + start as f32 * cw;
                    s.append_color(&rgba(bg), &Rect::new(left, top, (x - start) as f32 * cw, ch));
                }
            }

            self.draw_images(s, cells, top, cw, ch, pad);

            // Text, in runs of the same style; ASCII can share one layout.
            let mut run = String::new();
            let mut run_start = 0;
            let mut run_key = (0u8, (0u8, 0u8, 0u8));
            let flush = |run: &mut String, start: usize, key: (u8, Rgb)| {
                let text = run.trim_end_matches(' ');
                if !text.is_empty() {
                    let layout = font.layout(widget, key.0, text);
                    font.draw(s, &layout, text, pad + start as f32 * cw, top, key.1);
                }
                run.clear();
            };
            for (x, cell) in cells.iter().enumerate() {
                if cell.flags & flag::BLINK != 0 {
                    blinking = true;
                }
                if cell.is_wide_cont() || cell.tile().is_some() {
                    flush(&mut run, run_start, run_key);
                    continue;
                }
                let look = self.look(&term, cell, defaults, selected(x));
                let invisible = cell.flags & flag::HIDDEN != 0 || (cell.flags & flag::BLINK != 0 && !blink_on);
                let mut style = 0;
                if cell.flags & flag::BOLD != 0 {
                    style |= BOLD;
                }
                if cell.flags & flag::ITALIC != 0 {
                    style |= ITALIC;
                }
                let combining = cell.extra.as_ref().is_some_and(|e| !e.combining.is_empty());
                if !invisible {
                    self.draw_decorations(s, cell, x, top, look.fg, &font);
                }
                if invisible || (cell.c == ' ' && !combining) {
                    if run.is_empty() {
                        continue;
                    }
                    run.push(' ');
                } else if cell.c.is_ascii() && !combining {
                    let key = (style, look.fg);
                    if run.is_empty() || key != run_key {
                        flush(&mut run, run_start, run_key);
                        run_start = x;
                        run_key = key;
                    }
                    run.push(cell.c);
                } else {
                    flush(&mut run, run_start, run_key);
                    let mut text = String::new();
                    cell.push_text(&mut text);
                    flush(&mut text, x, (style, look.fg));
                }
            }
            flush(&mut run, run_start, run_key);
        }

        inner.blinking_cells.set(blinking);
        self.draw_cursor(s, &term, &palette, &font, defaults);
    }

    fn draw_images(&self, s: &gtk::Snapshot, cells: &[TermCell], top: f32, cw: f32, ch: f32, pad: f32) {
        let mut x = 0;
        while x < cells.len() {
            let Some(tile) = cells[x].tile() else {
                x += 1;
                continue;
            };
            // Consecutive tiles of an image are drawn in one go.
            let start = x;
            while x < cells.len()
                && cells[x].tile().is_some_and(|t| {
                    std::rc::Rc::ptr_eq(&t.image, &tile.image) && t.row == tile.row
                })
            {
                x += 1;
            }
            let image = &tile.image;
            let texture = image.texture.get_or_init(|| {
                gdk::MemoryTexture::new(
                    image.width as i32,
                    image.height as i32,
                    gdk::MemoryFormat::R8g8b8a8,
                    &image.rgba,
                    image.width as usize * 4,
                )
                .upcast()
            });
            // Pixels stay tied to the cells they were placed on, even if the font changed.
            let (sx, sy) = (cw / image.cell_w as f32, ch / image.cell_h as f32);
            let left = pad + start as f32 * cw;
            s.push_clip(&Rect::new(left, top, (x - start) as f32 * cw, ch));
            s.append_texture(texture, &Rect::new(
                left - tile.col as f32 * cw,
                top - tile.row as f32 * ch,
                image.width as f32 * sx,
                image.height as f32 * sy,
            ));
            s.pop();
        }
    }

    fn draw_decorations(&self, s: &gtk::Snapshot, cell: &TermCell, x: usize, top: f32, fg: Rgb, font: &Font) {
        let flags = cell.flags;
        if flags & (flag::UL_MASK | flag::STRIKE | flag::OVERLINE) == 0 {
            return;
        }
        let (cw, ch) = (font.cell_w as f32, font.cell_h as f32);
        let left = PADDING as f32 + x as f32 * cw;
        let color = rgba(fg);
        let bar = |l: f32, y: f32, w: f32| s.append_color(&color, &Rect::new(l, y, w, 1.0));
        let under = top + (font.baseline as f32 + 1.0).min(ch - 2.0);
        let ul_color = cell.extra.as_ref().and_then(|e| e.underline_color).map(|c| match c {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => self.inner().term.borrow().palette_color(i),
            Color::Default => fg,
        });
        let ul = rgba(ul_color.unwrap_or(fg));
        let ul_bar = |l: f32, y: f32, w: f32| s.append_color(&ul, &Rect::new(l, y, w, 1.0));
        match cell.underline() {
            cell::UL_SINGLE => ul_bar(left, under, cw),
            cell::UL_DOUBLE => {
                ul_bar(left, under, cw);
                ul_bar(left, under + 2.0, cw);
            }
            cell::UL_CURLY => {
                let quarter = cw / 4.0;
                for i in 0..4 {
                    let dy = if i == 1 || i == 2 { 1.0 } else { 0.0 };
                    ul_bar(left + i as f32 * quarter, under + dy, quarter);
                }
            }
            cell::UL_DOTTED => {
                let mut l = 0.0;
                while l < cw {
                    ul_bar(left + l, under, 1.0);
                    l += 3.0;
                }
            }
            cell::UL_DASHED => ul_bar(left, under, cw * 0.6),
            _ => {}
        }
        if flags & flag::STRIKE != 0 {
            bar(left, top + (font.baseline as f32 * 0.65).round(), cw);
        }
        if flags & flag::OVERLINE != 0 {
            bar(left, top, cw);
        }
    }

    fn draw_cursor(&self, s: &gtk::Snapshot, term: &Terminal, palette: &Palette, font: &Font, defaults: (Rgb, Rgb)) {
        let inner = self.inner();
        let Some((y, x)) = term.cursor_pos() else { return };
        let (cw, ch) = (font.cell_w as f32, font.cell_h as f32);
        let (left, top) = (PADDING as f32 + x as f32 * cw, PADDING as f32 + y as f32 * ch);
        inner.im.set_cursor_location(&gdk::Rectangle::new(left as i32, top as i32, font.cell_w, font.cell_h));

        let focused = inner.focused.get();
        if focused && term.cursor_blink && !inner.blink_on.get() {
            return;
        }
        let color = rgba(term.cursor_override.unwrap_or(palette.cursor));
        let row = term.display_row(y);
        let cell = row.cells.get(x);
        let wide = row.cells.get(x + 1).is_some_and(|c| c.is_wide_cont());
        let width = if wide { 2.0 * cw } else { cw };
        if !focused {
            for r in [
                Rect::new(left, top, width, 1.0),
                Rect::new(left, top + ch - 1.0, width, 1.0),
                Rect::new(left, top, 1.0, ch),
                Rect::new(left + width - 1.0, top, 1.0, ch),
            ] {
                s.append_color(&color, &r);
            }
            return;
        }
        match term.cursor_shape {
            CursorShape::Bar => s.append_color(&color, &Rect::new(left, top, 2.0, ch)),
            CursorShape::Underline => s.append_color(&color, &Rect::new(left, top + ch - 2.0, width, 2.0)),
            CursorShape::Block => {
                s.append_color(&color, &Rect::new(left, top, width, ch));
                // The character under the block, in the background colour.
                let Some(cell) = cell else { return };
                if cell.c == ' ' {
                    return;
                }
                let mut text = String::new();
                cell.push_text(&mut text);
                let mut style = 0;
                if cell.flags & flag::BOLD != 0 {
                    style |= BOLD;
                }
                if cell.flags & flag::ITALIC != 0 {
                    style |= ITALIC;
                }
                let layout = font.layout(self.upcast_ref(), style, &text);
                font.draw(s, &layout, &text, left, top, defaults.1);
            }
        }
    }
}

fn button_code(button: u32) -> Option<u32> {
    match button {
        gdk::BUTTON_PRIMARY => Some(0),
        gdk::BUTTON_MIDDLE => Some(1),
        gdk::BUTTON_SECONDARY => Some(2),
        _ => None,
    }
}
