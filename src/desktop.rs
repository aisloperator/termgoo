//! What the desktop has to say: its light/dark preference and its default font.
//!
//! For light and dark, GTK alone does not tell us: the setting lives in the XDG desktop portal, which
//! works the same on X11 and Wayland. Without a portal, the GTK theme's name and
//! the user's `gtk-application-prefer-dark-theme` setting are the best signals.

use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::Cell;
use std::rc::Rc;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const MONOSPACE_KEY: &str = "monospace-font-name";
const FALLBACK_FONT: &str = "Monospace 11";

/// GNOME's monospace font setting, and whether anyone actually set it. Missing
/// schemas are normal outside GNOME, and `gio::Settings` aborts on one, so look first.
fn gnome_monospace() -> Option<(String, bool)> {
    let schema = gio::SettingsSchemaSource::default()?.lookup(INTERFACE_SCHEMA, true)?;
    if !schema.has_key(MONOSPACE_KEY) {
        return None;
    }
    let settings = gio::Settings::new(INTERFACE_SCHEMA);
    Some((settings.string(MONOSPACE_KEY).to_string(), settings.user_value(MONOSPACE_KEY).is_some()))
}

/// The monospace font at the size of a GTK font name such as "Sans 10".
fn monospace_at_size_of(gtk_font: Option<&str>) -> String {
    let size = gtk_font.map(pango::FontDescription::from_string).filter(|d| d.size() > 0);
    let Some(gtk) = size else { return FALLBACK_FONT.to_string() };
    let mut font = pango::FontDescription::new();
    font.set_family("Monospace");
    if gtk.is_size_absolute() {
        font.set_absolute_size(gtk.size() as f64);
    } else {
        font.set_size(gtk.size());
    }
    font.to_string()
}

/// GNOME's setting is only a choice in a GNOME session, or if it was set on purpose.
/// Elsewhere it is an unused schema default, and GTK's own setting is what counts.
fn choose_font(session: &str, gnome: Option<(String, bool)>, gtk_font: Option<&str>) -> String {
    gnome
        .filter(|(name, explicit)| !name.is_empty() && (*explicit || session.contains("GNOME")))
        .map(|(name, _)| name)
        .unwrap_or_else(|| monospace_at_size_of(gtk_font))
}

/// The font to use when the user has not named one.
pub fn default_font() -> String {
    let session = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let gtk_font = gtk::Settings::default().and_then(|s| s.gtk_font_name());
    choose_font(&session, gnome_monospace(), gtk_font.as_deref())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn gnome(name: &str, explicit: bool) -> Option<(String, bool)> {
        Some((name.to_string(), explicit))
    }

    #[test]
    fn gnome_session_uses_the_gnome_setting_even_at_its_default() {
        assert_eq!(choose_font("GNOME", gnome("Source Code Pro 10", false), Some("Sans 14")), "Source Code Pro 10");
        assert_eq!(choose_font("ubuntu:GNOME", gnome("Ubuntu Mono 13", false), Some("Sans 14")), "Ubuntu Mono 13");
    }

    #[test]
    fn explicit_gnome_setting_counts_in_any_session() {
        assert_eq!(choose_font("sway", gnome("Iosevka 15", true), Some("Sans 14")), "Iosevka 15");
    }

    #[test]
    fn elsewhere_a_schema_default_gives_way_to_gtks_size() {
        assert_eq!(choose_font("sway", gnome("Monospace 11", false), Some("Sans 14")), "Monospace 14");
        assert_eq!(choose_font("", gnome("Monospace 11", false), Some("Noto Sans Bold 9.5")), "Monospace 9.5");
        assert_eq!(choose_font("XFCE", None, Some("Sans 12")), "Monospace 12");
    }

    #[test]
    fn a_gnome_session_without_a_usable_setting_uses_gtks_size() {
        assert_eq!(choose_font("GNOME", None, Some("Sans 12")), "Monospace 12");
        assert_eq!(choose_font("GNOME", gnome("", false), Some("Sans 12")), "Monospace 12");
    }

    #[test]
    fn absolute_gtk_sizes_stay_absolute() {
        assert_eq!(monospace_at_size_of(Some("Sans 20px")), "Monospace 20px");
    }

    #[test]
    fn no_size_to_go_on_falls_back() {
        assert_eq!(choose_font("", None, None), "Monospace 11");
        assert_eq!(choose_font("", None, Some("Sans")), "Monospace 11");
    }
}
