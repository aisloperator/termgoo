//! The window: the terminal, the optional scrollbar, and the context menu that
//! holds every command.

use crate::cli::Options;
use crate::prefs;
use crate::config::{Config, ThemeChoice};
use crate::desktop;
use crate::view::{Settings, TermView};
use gtk::gio::prelude::*;
use gtk::glib::{self, clone};
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, gio, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One process, one window. Tabs and multiplexing are deliberately absent.
struct State {
    window: ApplicationWindow,
    view: TermView,
    scrollbar: gtk::Scrollbar,
    config: RefCell<Config>,
    desktop_dark: Cell<bool>,
    prefs: RefCell<Option<gtk::Window>>,
    /// Percent of the font's size, for this window only. Not saved.
    zoom: Cell<i32>,
}

impl State {
    fn apply_palette(&self) {
        let cfg = self.config.borrow();
        let dark = match cfg.theme {
            ThemeChoice::Follow => self.desktop_dark.get(),
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
        };
        self.view.set_palette(if dark { cfg.dark.clone() } else { cfg.light.clone() });
    }

    /// Change the settings and put into effect what changed. TERM is left alone:
    /// the program running already has it.
    fn update(&self, change: &dyn Fn(&mut Config)) {
        let old = self.config.borrow().clone();
        let mut new = old.clone();
        change(&mut new);
        *self.config.borrow_mut() = new.clone();
        if new.font != old.font {
            self.view.set_font(&zoomed_font(&font_name(&new), self.zoom.get()));
        }
        if new.scrollback != old.scrollback {
            self.view.set_scrollback(new.scrollback);
        }
        if new.scrollbar != old.scrollbar {
            self.scrollbar.set_visible(new.scrollbar);
        }
        if (new.theme, &new.light, &new.dark) != (old.theme, &old.light, &old.dark) {
            self.apply_palette();
        }
    }

    fn set_zoom(&self, percent: i32) {
        let percent = percent.clamp(ZOOM_LEVELS[0], ZOOM_LEVELS[ZOOM_LEVELS.len() - 1]);
        if percent != self.zoom.replace(percent) {
            self.view.set_font(&zoomed_font(&font_name(&self.config.borrow()), percent));
        }
    }

    fn show_prefs(self: &Rc<Self>) {
        if let Some(w) = &*self.prefs.borrow() {
            return w.present();
        }
        let weak = Rc::downgrade(self);
        let apply: prefs::Apply = Rc::new(move |change| {
            if let Some(state) = weak.upgrade() {
                state.update(change);
            }
        });
        let w = prefs::window(&self.window, &self.config.borrow(), apply);
        w.connect_close_request(clone!(#[weak(rename_to = state)] self, #[upgrade_or] glib::Propagation::Proceed, move |_| {
            state.prefs.take();
            glib::Propagation::Proceed
        }));
        w.present();
        *self.prefs.borrow_mut() = Some(w);
    }
}

/// The font asked for, or else the desktop's.
fn font_name(config: &Config) -> String {
    config.font.clone().unwrap_or_else(desktop::default_font)
}

/// The zoom percentages offered on the menu and stepped through from the keyboard.
const ZOOM_LEVELS: [i32; 13] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// The next zoom level up or down from `percent`.
fn zoom_step(percent: i32, up: bool) -> i32 {
    let next = if up {
        ZOOM_LEVELS.iter().find(|&&l| l > percent)
    } else {
        ZOOM_LEVELS.iter().rev().find(|&&l| l < percent)
    };
    next.copied().unwrap_or(percent)
}

/// A font name with its size scaled to `percent`. A name with no size gets the
/// usual default of 11 points scaled.
fn zoomed_font(name: &str, percent: i32) -> String {
    if percent == 100 {
        return name.to_string();
    }
    let mut d = pango::FontDescription::from_string(name);
    let scale = |size: i32| ((size as f64) * percent as f64 / 100.0).round().max(1.0);
    if d.size() <= 0 {
        d.set_size(scale(11 * pango::SCALE) as i32);
    } else if d.is_size_absolute() {
        d.set_absolute_size(scale(d.size()));
    } else {
        d.set_size(scale(d.size()) as i32);
    }
    d.to_string()
}

fn menu_item(label: &str, action: &str, accel: Option<&str>) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    if let Some(accel) = accel {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    item
}

fn build_zoom_menu() -> gio::Menu {
    let zoom = gio::Menu::new();
    let steps = gio::Menu::new();
    steps.append_item(&menu_item("Zoom In", "win.zoom-in", Some("<Control>plus")));
    steps.append_item(&menu_item("Zoom Out", "win.zoom-out", Some("<Control>minus")));
    steps.append_item(&menu_item("Normal Size", "win.zoom-reset", Some("<Control>0")));
    zoom.append_section(None, &steps);
    let levels = gio::Menu::new();
    for level in ZOOM_LEVELS {
        let item = gio::MenuItem::new(Some(&format!("{level}%")), None);
        item.set_action_and_target_value(Some("win.zoom"), Some(&level.to_variant()));
        levels.append_item(&item);
    }
    zoom.append_section(None, &levels);
    zoom
}

fn build_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let edit = gio::Menu::new();
    edit.append_item(&menu_item("Copy", "win.copy", Some("<Control><Shift>c")));
    edit.append_item(&menu_item("Paste", "win.paste", Some("<Control><Shift>v")));
    edit.append(Some("Select All"), Some("win.select-all"));
    menu.append_section(None, &edit);

    let terminal = gio::Menu::new();
    terminal.append(Some("Clear Scrollback"), Some("win.clear-scrollback"));
    terminal.append(Some("Reset Terminal"), Some("win.reset"));
    menu.append_section(None, &terminal);

    let settings = gio::Menu::new();
    settings.append_submenu(Some("Zoom"), &build_zoom_menu());
    settings.append(Some("Preferences"), Some("win.preferences"));
    menu.append_section(None, &settings);

    let last = gio::Menu::new();
    last.append(Some("About TermGoo"), Some("win.about"));
    last.append(Some("Close Window"), Some("win.close"));
    menu.append_section(None, &last);
    menu
}

fn show_about(window: &ApplicationWindow) {
    gtk::AboutDialog::builder()
        .transient_for(window)
        .modal(true)
        .program_name("TermGoo")
        .logo_icon_name("utilities-terminal")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Created by AI Sloperator with Claude Code\n\nNo license is given.")
        .build()
        .present();
}

fn add_actions(state: &Rc<State>) {
    let window = &state.window;
    let simple = |name: &str, f: Box<dyn Fn()>| {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| f());
        window.add_action(&action);
        action
    };
    let view = &state.view;
    let copy = simple("copy", Box::new(clone!(#[strong] view, move || view.copy())));
    simple("paste", Box::new(clone!(#[strong] view, move || view.paste())));
    simple("select-all", Box::new(clone!(#[strong] view, move || view.select_all())));
    simple("clear-scrollback", Box::new(clone!(#[strong] view, move || view.clear_scrollback())));
    simple("reset", Box::new(clone!(#[strong] view, move || view.reset())));
    let zoom = gio::SimpleAction::new_stateful("zoom", Some(glib::VariantTy::INT32), &100.to_variant());
    zoom.connect_change_state(clone!(#[weak] state, move |action, value| {
        if let Some(percent) = value.and_then(|v| v.get::<i32>()) {
            state.set_zoom(percent);
            action.set_state(&state.zoom.get().to_variant());
        }
    }));
    window.add_action(&zoom);
    let step = |up: bool| {
        Box::new(clone!(#[strong] zoom, #[weak] state, move || {
            zoom.change_state(&zoom_step(state.zoom.get(), up).to_variant());
        })) as Box<dyn Fn()>
    };
    simple("zoom-in", step(true));
    simple("zoom-out", step(false));
    simple("zoom-reset", Box::new(clone!(#[strong] zoom, move || zoom.change_state(&100.to_variant()))));
    simple("preferences", Box::new(clone!(#[strong] state, move || state.show_prefs())));
    simple("about", Box::new(clone!(#[strong] window, move || show_about(&window))));
    simple("close", Box::new(clone!(#[strong] window, move || window.close())));

    state.view.set_menu(&build_menu(), clone!(#[strong] view, move || {
        copy.set_enabled(view.has_selection());
    }));
}

/// Build and show the window. Failing to start the program is fatal: the error goes
/// to the terminal this was started from, as there would be nothing to show in a window.
pub fn open(app: &Application, options: &Options) -> Result<(), String> {
    let mut config = Config::load();
    options.apply(&mut config);

    let palette = config.dark.clone();
    let view = TermView::new(Settings {
        command: options.command.as_deref(),
        term: &config.term,
        font: &font_name(&config),
        size: options.geometry.unwrap_or((80, 24)),
        scrollback: config.scrollback,
        palette,
        hold: options.hold,
    })
    .map_err(|e| e.to_string())?;

    let scrollbar = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&view.adjustment()));
    scrollbar.set_visible(config.scrollbar);
    view.setup_scrollbar_link();

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&view);
    content.append(&scrollbar);

    let window = ApplicationWindow::builder()
        .application(app)
        .title(options.window_title())
        .child(&content)
        .build();

    view.connect_title(clone!(#[weak] window, move |title| window.set_title(Some(title))));
    view.connect_exit(clone!(#[weak] window, move || window.close()));

    let state = Rc::new(State {
        window: window.clone(),
        view: view.clone(),
        scrollbar,
        config: RefCell::new(config),
        desktop_dark: Cell::new(false),
        prefs: RefCell::new(None),
        zoom: Cell::new(100),
    });
    add_actions(&state);

    desktop::watch(clone!(#[strong] state, move |dark| {
        state.desktop_dark.set(dark);
        state.apply_palette();
    }));

    window.present();
    view.grab_focus();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_through_the_levels() {
        assert_eq!(zoom_step(100, true), 110);
        assert_eq!(zoom_step(100, false), 90);
        assert_eq!(zoom_step(300, true), 300);
        assert_eq!(zoom_step(50, false), 50);
        assert_eq!(zoom_step(105, true), 110);
        assert_eq!(zoom_step(105, false), 100);
    }

    #[test]
    fn zoom_scales_the_font_size() {
        assert_eq!(zoomed_font("Monospace 11", 100), "Monospace 11");
        assert_eq!(zoomed_font("Monospace 10", 150), "Monospace 15");
        assert_eq!(zoomed_font("DejaVu Sans Mono Bold 12", 50), "DejaVu Sans Mono Bold 6");
        assert_eq!(zoomed_font("Monospace", 200), "Monospace 22");
        assert_eq!(zoomed_font("Monospace 20px", 50), "Monospace 10px");
    }
}
