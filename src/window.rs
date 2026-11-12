//! The window: the terminal, the optional scrollbar, and the context menu that
//! holds every command.

use crate::cli::Options;
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
        self.view.set_font(&font_name(&cfg));
        self.view.set_scrollback(cfg.scrollback);
        self.scrollbar.set_visible(cfg.scrollbar);
        self.theme.set(cfg.theme);
        set_action_state(&self.window, "scrollbar", &cfg.scrollbar.to_variant());
        set_action_state(&self.window, "theme", &cfg.theme.name().to_variant());
        *self.config.borrow_mut() = cfg;
        self.apply_palette();
    }
}

/// The font asked for, or else the desktop's.
fn font_name(config: &Config) -> String {
    config.font.clone().unwrap_or_else(desktop::default_font)
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

/// The user's editor, as terminal programs look for it.
fn editor_from_env() -> Option<String> {
    ["VISUAL", "EDITOR"].into_iter().find_map(|v| std::env::var(v).ok().filter(|s| !s.trim().is_empty()))
}

/// Open the file in the user's editor in a new TermGoo window. As in Git, the
/// setting is shell code, so it may have arguments and quoting. The path travels
/// as an argument, so it needs no quoting of its own. If the editor fails, the
/// window stays open to say so.
fn edit_in_terminal(editor: &str, path: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let script = format!(
        "{editor} \"$@\"\nstatus=$?\n\
         [ $status -eq 0 ] || {{ printf '\\nThe editor exited with status %s. Press Enter to close.' $status; read _; }}"
    );
    let argv = [
        exe.as_os_str(),
        "-title".as_ref(),
        "TermGoo configuration".as_ref(),
        "-e".as_ref(),
        "/bin/sh".as_ref(),
        "-c".as_ref(),
        script.as_ref(),
        "sh".as_ref(),
        path.as_os_str(),
    ];
    // GLib waits for the process, so it does not linger after it exits.
    gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE).map(drop).map_err(|e| e.to_string())
}

/// Open the file with the desktop's program for plain text. This asks for that
/// type directly: opening the file's URI would first ask for a handler of the
/// `file:` scheme, which an installed program can claim for every file.
fn open_as_text(path: &std::path::Path) -> Result<(), String> {
    let app = gio::AppInfo::default_for_type("text/plain", false)
        .ok_or("no program is set up to open plain text")?;
    app.launch(&[gio::File::for_path(path)], None::<&gio::AppLaunchContext>).map_err(|e| e.to_string())
}

/// As Debian Policy 11.4 asks of programs that start an editor, and as other
/// terminals do, prefer `$VISUAL` and then `$EDITOR`. Only without them does the
/// desktop choose.
fn open_config() {
    let path = match Config::ensure_file() {
        Ok(path) => path,
        Err(e) => return eprintln!("termgoo: cannot create the configuration file: {e}"),
    };
    let result = match editor_from_env() {
        Some(editor) => edit_in_terminal(&editor, &path),
        None => open_as_text(&path),
    };
    if let Err(e) = result {
        eprintln!("termgoo: cannot open {}: {e}", path.display());
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
        options: options.clone(),
        theme: Cell::new(config.theme),
        config: RefCell::new(config),
        desktop_dark: Cell::new(false),
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
