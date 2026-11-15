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
use gtk::{Application, ApplicationWindow, gio};
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
            self.view.set_font(&font_name(&new));
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

fn build_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let edit = gio::Menu::new();
    edit.append(Some("Copy"), Some("win.copy"));
    edit.append(Some("Paste"), Some("win.paste"));
    edit.append(Some("Select All"), Some("win.select-all"));
    menu.append_section(None, &edit);

    let terminal = gio::Menu::new();
    terminal.append(Some("Clear Scrollback"), Some("win.clear-scrollback"));
    terminal.append(Some("Reset Terminal"), Some("win.reset"));
    menu.append_section(None, &terminal);

    let settings = gio::Menu::new();
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
