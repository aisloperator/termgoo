//! Following the desktop's light/dark preference.
//!
//! GTK alone does not tell us: the setting lives in the XDG desktop portal, which
//! works the same on X11 and Wayland. Without a portal, the GTK theme's name and
//! the user's `gtk-application-prefer-dark-theme` setting are the best signals.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

struct Tracker {
    settings: gtk::Settings,
    /// What the portal says, if it has an opinion.
    portal_dark: Cell<Option<bool>>,
    /// The user's GTK setting from before we began to manage it.
    configured_dark: bool,
    current: Cell<Option<bool>>,
    on_change: Box<dyn Fn(bool)>,
}

impl Tracker {
    fn update(&self) {
        let theme_is_dark = || {
            self.settings.gtk_theme_name().is_some_and(|n| n.to_lowercase().ends_with("-dark"))
        };
        let dark = self.portal_dark.get().unwrap_or_else(|| self.configured_dark || theme_is_dark());
        if self.current.replace(Some(dark)) != Some(dark) {
            // This is what makes the scrollbar and menus match the desktop.
            self.settings.set_gtk_application_prefer_dark_theme(dark);
            (self.on_change)(dark);
        }
    }
}

/// The portal's value may be wrapped in one or two layers of variant.
fn scheme_from(mut v: glib::Variant) -> Option<bool> {
    while v.is_type(glib::VariantTy::VARIANT) {
        v = v.as_variant()?;
    }
    match v.get::<u32>()? {
        1 => Some(true),
        2 => Some(false),
        _ => None, // 0 is "no preference".
    }
}

fn read_portal(bus: &gio::DBusConnection) -> Option<bool> {
    let reply = bus
        .call_sync(
            Some(PORTAL),
            PATH,
            INTERFACE,
            "Read",
            Some(&(NAMESPACE, KEY).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            500,
            None::<&gio::Cancellable>,
        )
        .ok()?;
    scheme_from(reply.child_value(0))
}

/// Call `on_change` now with the current preference, and again on every change.
pub fn watch(on_change: impl Fn(bool) + 'static) {
    let settings = gtk::Settings::default().expect("display is open");
    let tracker = Rc::new(Tracker {
        configured_dark: settings.is_gtk_application_prefer_dark_theme(),
        settings: settings.clone(),
        portal_dark: Cell::new(None),
        current: Cell::new(None),
        on_change: Box::new(on_change),
    });

    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
        tracker.portal_dark.set(read_portal(&bus));
        let t = tracker.clone();
        let subscription = bus.subscribe_to_signal(
            Some(PORTAL),
            Some(INTERFACE),
            Some("SettingChanged"),
            Some(PATH),
            Some(NAMESPACE),
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let (ns, key, value) = (
                    signal.parameters.child_value(0),
                    signal.parameters.child_value(1),
                    signal.parameters.child_value(2),
                );
                if ns.str() == Some(NAMESPACE) && key.str() == Some(KEY) {
                    t.portal_dark.set(scheme_from(value));
                    t.update();
                }
            },
        );
        // The watch is for the life of the process.
        std::mem::forget(subscription);
        std::mem::forget(bus);
    }

    let t = tracker.clone();
    settings.connect_gtk_theme_name_notify(move |_| t.update());
    tracker.update();
}
