//! The Preferences window. It has a control for every setting in the
//! configuration file. Each change applies to the terminal window at once and
//! is written to the file, so that later windows start with it too.

use crate::config::{Config, MAX_SCROLLBACK, Palette, Rgb, ThemeChoice, hex};
use crate::desktop;
use gtk::prelude::*;
use gtk::{gdk, pango};
use std::rc::Rc;

/// Changes the running window's settings. The Preferences window saves to the
/// file itself.
pub type Apply = Rc<dyn Fn(&dyn Fn(&mut Config))>;

const THEMES: [ThemeChoice; 3] = [ThemeChoice::Follow, ThemeChoice::Light, ThemeChoice::Dark];
const COLOR_NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

fn save(section: &str, key: &str, value: Option<&str>) {
    if let Err(e) = Config::store(section, key, value) {
        eprintln!("termgoo: cannot save {key}: {e}");
    }
}

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).build()
}

fn note(text: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).build();
    l.add_css_class("dim-label");
    l
}

fn grid() -> gtk::Grid {
    gtk::Grid::builder()
        .row_spacing(10)
        .column_spacing(16)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build()
}

fn rgba((r, g, b): Rgb) -> gdk::RGBA {
    gdk::RGBA::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

fn rgb(c: &gdk::RGBA) -> Rgb {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (byte(c.red()), byte(c.green()), byte(c.blue()))
}

fn general_page(config: &Config, apply: &Apply) -> gtk::Grid {
    let g = grid();
    let mut row = 0;
    let mut add = |name: &str, widget: &gtk::Widget, help: Option<&str>| {
        g.attach(&label(name), 0, row, 1, 1);
        g.attach(widget, 1, row, 1, 1);
        row += 1;
        if let Some(help) = help {
            g.attach(&note(help), 1, row, 1, 1);
            row += 1;
        }
    };

    let theme = gtk::DropDown::from_strings(&["Follow the desktop", "Light", "Dark"]);
    theme.set_selected(THEMES.iter().position(|t| *t == config.theme).unwrap_or(0) as u32);
    theme.connect_selected_notify({
        let apply = apply.clone();
        move |d| {
            let Some(&choice) = THEMES.get(d.selected() as usize) else { return };
            apply(&|c| c.theme = choice);
            save("", "theme", Some(choice.name()));
        }
    });
    add("Terminal theme", theme.upcast_ref(), None);

    let use_desktop = gtk::Switch::builder().active(config.font.is_none()).halign(gtk::Align::Start).build();
    // A terminal needs fonts with every character the same width.
    let monospace = gtk::CustomFilter::new(|item| {
        let family = item
            .downcast_ref::<pango::FontFamily>()
            .cloned()
            .or_else(|| item.downcast_ref::<pango::FontFace>().map(|f| f.family()));
        family.is_some_and(|f| f.is_monospace())
    });
    let dialog = gtk::FontDialog::builder().title("Terminal Font").filter(&monospace).build();
    let font = gtk::FontDialogButton::new(Some(dialog));
    font.set_font_desc(&pango::FontDescription::from_string(
        &config.font.clone().unwrap_or_else(desktop::default_font),
    ));
    font.set_sensitive(config.font.is_some());
    add("Use the desktop's font", use_desktop.upcast_ref(), None);
    add("Font", font.upcast_ref(), None);
    let set_font = {
        let apply = apply.clone();
        move |name: Option<String>| {
            apply(&|c| c.font = name.clone());
            save("", "font", name.as_deref());
        }
    };
    use_desktop.connect_active_notify({
        let (font, set_font) = (font.clone(), set_font.clone());
        move |s| {
            font.set_sensitive(!s.is_active());
            if s.is_active() {
                font.set_font_desc(&pango::FontDescription::from_string(&desktop::default_font()));
                set_font(None);
            } else {
                set_font(font.font_desc().map(|d| d.to_string()));
            }
        }
    });
    font.connect_font_desc_notify(move |b| {
        if b.is_sensitive() {
            set_font(b.font_desc().map(|d| d.to_string()));
        }
    });

    let scrollback = gtk::SpinButton::with_range(0.0, MAX_SCROLLBACK as f64, 1000.0);
    scrollback.set_digits(0);
    scrollback.set_value(config.scrollback as f64);
    scrollback.set_halign(gtk::Align::Start);
    scrollback.connect_value_changed({
        let apply = apply.clone();
        move |s| {
            let lines = s.value_as_int().max(0) as usize;
            apply(&|c| c.scrollback = lines);
            save("", "scrollback", Some(&lines.to_string()));
        }
    });
    add("Lines of scrollback", scrollback.upcast_ref(), None);

    let scrollbar = gtk::Switch::builder().active(config.scrollbar).halign(gtk::Align::Start).build();
    scrollbar.connect_active_notify({
        let apply = apply.clone();
        move |s| {
            let on = s.is_active();
            apply(&|c| c.scrollbar = on);
            save("", "scrollbar", Some(if on { "true" } else { "false" }));
        }
    });
    add("Show the scrollbar", scrollbar.upcast_ref(), None);

    let term = gtk::Entry::builder().text(config.term.as_str()).hexpand(true).build();
    term.connect_changed({
        let apply = apply.clone();
        move |e| {
            let value = e.text().trim().to_string();
            if value.is_empty() || value.contains(char::is_whitespace) {
                return;
            }
            apply(&|c| c.term = value.clone());
            save("", "term", Some(&value));
        }
    });
    add(
        "TERM",
        term.upcast_ref(),
        Some("What programs are told the terminal is. It applies to windows\nopened after the change. The emulation stays the same whatever it says."),
    );
    g
}

/// One colour button per palette entry, keyed as in the file.
fn palette_page(section: &'static str, palette: &Palette, defaults: Palette, apply: &Apply) -> gtk::Grid {
    let g = grid();
    let dialog = gtk::ColorDialog::builder().with_alpha(false).modal(true).build();
    let mut buttons: Vec<(String, gtk::ColorDialogButton, Rgb)> = Vec::new();
    let mut button = |key: String, color: Rgb, default: Rgb, tip: &str| {
        let b = gtk::ColorDialogButton::new(Some(dialog.clone()));
        b.set_rgba(&rgba(color));
        b.set_tooltip_text(Some(tip));
        b.set_halign(gtk::Align::Start);
        let apply = apply.clone();
        let k = key.clone();
        b.connect_rgba_notify(move |b| {
            let c = rgb(&b.rgba());
            apply(&|cfg| {
                let p = if section == "light" { &mut cfg.light } else { &mut cfg.dark };
                p.set(&k, c);
            });
            save(section, &k, Some(&hex(c)));
        });
        buttons.push((key, b.clone(), default));
        b
    };

    let named = [
        ("Foreground", "foreground", palette.foreground, defaults.foreground),
        ("Background", "background", palette.background, defaults.background),
        ("Cursor", "cursor", palette.cursor, defaults.cursor),
        ("Selection", "selection", palette.selection, defaults.selection),
    ];
    for (row, (name, key, color, default)) in named.into_iter().enumerate() {
        g.attach(&label(name), 0, row as i32, 1, 1);
        g.attach(&button(key.to_string(), color, default, name), 1, row as i32, 8, 1);
    }
    for (row, (name, offset)) in [("Normal colors", 0), ("Bright colors", 8)].into_iter().enumerate() {
        let row = 4 + row as i32;
        g.attach(&label(name), 0, row, 1, 1);
        for (i, color_name) in COLOR_NAMES.iter().enumerate() {
            let n = offset + i;
            let bright = if offset == 8 { "bright " } else { "" };
            let tip = format!("Color {n}: {bright}{color_name}");
            g.attach(&button(format!("color{n}"), palette.colors[n], defaults.colors[n], &tip), 1 + i as i32, row, 1, 1);
        }
    }

    let reset = gtk::Button::builder().label("Restore Defaults").halign(gtk::Align::Start).margin_top(8).build();
    reset.connect_clicked(move |_| {
        for (_, b, default) in &buttons {
            if rgb(&b.rgba()) != *default {
                b.set_rgba(&rgba(*default));
            }
        }
    });
    g.attach(&reset, 0, 6, 9, 1);
    g
}

pub fn window(parent: &impl IsA<gtk::Window>, config: &Config, apply: Apply) -> gtk::Window {
    let notebook = gtk::Notebook::new();
    notebook.append_page(&general_page(config, &apply), Some(&gtk::Label::new(Some("General"))));
    notebook.append_page(
        &palette_page("light", &config.light, Palette::light(), &apply),
        Some(&gtk::Label::new(Some("Light Theme Colors"))),
    );
    notebook.append_page(
        &palette_page("dark", &config.dark, Palette::dark(), &apply),
        Some(&gtk::Label::new(Some("Dark Theme Colors"))),
    );
    let window = gtk::Window::builder()
        .title("TermGoo Preferences")
        .transient_for(parent)
        .destroy_with_parent(true)
        .resizable(false)
        .child(&notebook)
        .build();
    // Every change is already in effect, so Escape just closes, as in a dialog.
    let keys = gtk::ShortcutController::new();
    keys.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.add_controller(keys);
    window
}
