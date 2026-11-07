//! The window: the terminal, the optional scrollbar, and the context menu that
//! holds every command.

use crate::cli::Options;
use crate::config::{Config, ThemeChoice};
use crate::desktop;
use crate::view::{Settings, TermView};
use gtk::gdk::{self, SurfaceEdge};
use gtk::gio::prelude::*;
use gtk::glib::{self, clone};
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, gio};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Width of the band along the edge of a window without a title bar that resizes it.
const GRIP: f64 = 4.0;

/// One process, one window. Tabs and multiplexing are deliberately absent.
struct State {
    window: ApplicationWindow,
    view: TermView,
    scrollbar: gtk::Scrollbar,
    options: Options,
    config: RefCell<Config>,
    theme: Cell<ThemeChoice>,
    desktop_dark: Cell<bool>,
}

impl State {
    fn apply_palette(&self) {
        let dark = match self.theme.get() {
            ThemeChoice::Follow => self.desktop_dark.get(),
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
        };
        let cfg = self.config.borrow();
        self.view.set_palette(if dark { cfg.dark.clone() } else { cfg.light.clone() });
    }

    fn reload(&self) {
        let mut cfg = Config::load();
        self.options.apply(&mut cfg);
        self.view.set_font(&cfg.font);
        self.view.set_scrollback(cfg.scrollback);
        self.scrollbar.set_visible(cfg.scrollbar);
        set_titlebar(&self.window, cfg.titlebar);
        self.theme.set(cfg.theme);
        set_action_state(&self.window, "scrollbar", &cfg.scrollbar.to_variant());
        set_action_state(&self.window, "titlebar", &cfg.titlebar.to_variant());
        set_action_state(&self.window, "theme", &cfg.theme.name().to_variant());
        *self.config.borrow_mut() = cfg;
        self.apply_palette();
    }
}

/// GTK settles how a window is decorated when it is realized, so a change takes
/// a new surface. The menu's click is still being handled now, hence the idle.
fn set_titlebar(window: &ApplicationWindow, on: bool) {
    if window.is_decorated() == on {
        return;
    }
    glib::idle_add_local_once(clone!(#[weak] window, move || {
        let (width, height) = (window.width(), window.height());
        window.set_visible(false);
        WidgetExt::unrealize(&window);
        window.set_decorated(on);
        window.set_default_size(width, height);
        window.present();
    }));
}

fn set_action_state(window: &ApplicationWindow, name: &str, value: &glib::Variant) {
    if let Some(action) = window.lookup_action(name) {
        action.change_state(value);
    }
}

fn save(key: &str, value: &str) {
    if let Err(e) = Config::store(key, value) {
        eprintln!("termgoo: cannot save {key}: {e}");
    }
}

fn build_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let edit = gio::Menu::new();
    edit.append(Some("Copy"), Some("win.copy"));
    edit.append(Some("Paste"), Some("win.paste"));
    edit.append(Some("Select All"), Some("win.select-all"));
    menu.append_section(None, &edit);

    let theme = gio::Menu::new();
    theme.append(Some("Follow Desktop"), Some("win.theme::follow"));
    theme.append(Some("Light"), Some("win.theme::light"));
    theme.append(Some("Dark"), Some("win.theme::dark"));
    let view = gio::Menu::new();
    view.append_submenu(Some("Terminal Theme"), &theme);
    view.append(Some("Show Scrollbar"), Some("win.scrollbar"));
    view.append(Some("Show Title Bar"), Some("win.titlebar"));
    menu.append_section(None, &view);

    let terminal = gio::Menu::new();
    terminal.append(Some("Clear Scrollback"), Some("win.clear-scrollback"));
    terminal.append(Some("Reset Terminal"), Some("win.reset"));
    menu.append_section(None, &terminal);

    let settings = gio::Menu::new();
    settings.append(Some("Open Configuration File"), Some("win.open-config"));
    settings.append(Some("Reload Configuration"), Some("win.reload-config"));
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

fn open_config() {
    match Config::ensure_file() {
        Ok(path) => {
            let uri = gio::File::for_path(&path).uri();
            if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
                eprintln!("termgoo: cannot open {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("termgoo: cannot create the configuration file: {e}"),
    }
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
    simple("open-config", Box::new(open_config));
    simple("reload-config", Box::new(clone!(#[strong] state, move || state.reload())));
    simple("about", Box::new(clone!(#[strong] window, move || show_about(&window))));
    simple("close", Box::new(clone!(#[strong] window, move || window.close())));

    let toggle = |name: &'static str, initial: bool, f: Box<dyn Fn(bool)>| {
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        action.connect_change_state(move |action, value| {
            if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                action.set_state(&on.to_variant());
                f(on);
            }
        });
        // Activation is the menu's doing, so unlike a reload it updates the file.
        action.connect_activate(move |action, _| {
            let on = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            action.change_state(&on.to_variant());
            save(name, if on { "true" } else { "false" });
        });
        window.add_action(&action);
    };
    let cfg = state.config.borrow();
    toggle("scrollbar", cfg.scrollbar, Box::new(clone!(
        #[strong] state, move |on| state.scrollbar.set_visible(on)
    )));
    toggle("titlebar", cfg.titlebar, Box::new(clone!(
        #[strong] window, move |on| set_titlebar(&window, on)
    )));

    let theme = gio::SimpleAction::new_stateful(
        "theme",
        Some(glib::VariantTy::STRING),
        &state.theme.get().name().to_variant(),
    );
    theme.connect_change_state(clone!(#[strong] state, move |action, value| {
        let Some(name) = value.and_then(|v| v.str()) else { return };
        let Some(choice) = ThemeChoice::parse(name) else { return };
        action.set_state(&name.to_variant());
        state.theme.set(choice);
        state.apply_palette();
    }));
    theme.connect_activate(|action, value| {
        if let Some(value) = value {
            action.change_state(value);
            save("theme", value.str().unwrap_or("follow"));
        }
    });
    window.add_action(&theme);

    state.view.set_menu(&build_menu(), clone!(#[strong] view, move || {
        copy.set_enabled(view.has_selection());
    }));
}

/// Which resize edge a point is on, if the window has no frame to do that for it.
fn edge_at(x: f64, y: f64, w: f64, h: f64) -> Option<SurfaceEdge> {
    let (left, right) = (x < GRIP, x >= w - GRIP);
    let (top, bottom) = (y < GRIP, y >= h - GRIP);
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => SurfaceEdge::NorthWest,
        (_, true, true, _) => SurfaceEdge::NorthEast,
        (true, _, _, true) => SurfaceEdge::SouthWest,
        (_, true, _, true) => SurfaceEdge::SouthEast,
        (true, ..) => SurfaceEdge::West,
        (_, true, ..) => SurfaceEdge::East,
        (_, _, true, _) => SurfaceEdge::North,
        (_, _, _, true) => SurfaceEdge::South,
        _ => return None,
    })
}

fn edge_cursor(edge: SurfaceEdge) -> &'static str {
    match edge {
        SurfaceEdge::NorthWest => "nw-resize",
        SurfaceEdge::NorthEast => "ne-resize",
        SurfaceEdge::SouthWest => "sw-resize",
        SurfaceEdge::SouthEast => "se-resize",
        SurfaceEdge::West => "w-resize",
        SurfaceEdge::East => "e-resize",
        SurfaceEdge::North => "n-resize",
        _ => "s-resize",
    }
}

/// With no title bar there is no frame either, so the window edge does its job.
fn add_resize_grips(window: &ApplicationWindow) {
    let size = |w: &ApplicationWindow| (w.width() as f64, w.height() as f64);

    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    motion.connect_motion(clone!(#[weak] window, move |_, x, y| {
        let (w, h) = size(&window);
        let edge = if window.is_decorated() { None } else { edge_at(x, y, w, h) };
        window.set_cursor_from_name(edge.map(edge_cursor));
    }));
    window.add_controller(motion);

    let press = gtk::GestureClick::new();
    press.set_button(gdk::BUTTON_PRIMARY);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    press.connect_pressed(clone!(#[weak] window, move |gesture, _, x, y| {
        if window.is_decorated() {
            return;
        }
        let (w, h) = size(&window);
        let Some(edge) = edge_at(x, y, w, h) else { return };
        let Some(surface) = window.surface().and_downcast::<gdk::Toplevel>() else { return };
        let Some(device) = gesture.device() else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        surface.begin_resize(edge, Some(&device), gdk::BUTTON_PRIMARY as i32, x, y, gesture.current_event_time());
    }));
    window.add_controller(press);
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
        font: &config.font,
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
        .decorated(config.titlebar)
        .child(&content)
        .build();

    view.connect_title(clone!(#[weak] window, move |title| window.set_title(Some(title))));
    view.connect_exit(clone!(#[weak] window, move || window.close()));

    let state = Rc::new(State {
        window: window.clone(),
        view: view.clone(),
        scrollbar,
        options: options.clone(),
        theme: Cell::new(config.theme),
        config: RefCell::new(config),
        desktop_dark: Cell::new(false),
    });
    add_actions(&state);
    add_resize_grips(&window);

    desktop::watch(clone!(#[strong] state, move |dark| {
        state.desktop_dark.set(dark);
        state.apply_palette();
    }));

    window.present();
    view.grab_focus();
    Ok(())
}
