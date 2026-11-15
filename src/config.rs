//! The configuration file: `$XDG_CONFIG_HOME/termgoo/config`, by default
//! `~/.config/termgoo/config`. Plain `key = value` lines, with `[light]` and
//! `[dark]` sections for the two terminal themes.

use crate::term::{ThemeColors, parse_color};
use std::path::PathBuf;
use std::{env, fs};

pub type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeChoice {
    Follow,
    Light,
    Dark,
}

impl ThemeChoice {
    pub fn parse(s: &str) -> Option<ThemeChoice> {
        match s {
            "follow" => Some(ThemeChoice::Follow),
            "light" => Some(ThemeChoice::Light),
            "dark" => Some(ThemeChoice::Dark),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ThemeChoice::Follow => "follow",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub colors: [Rgb; 16],
}

impl Palette {
    pub fn light() -> Palette {
        Palette {
            foreground: (0x2e, 0x34, 0x36),
            background: (0xfa, 0xfa, 0xf7),
            cursor: (0x2e, 0x34, 0x36),
            selection: (0xb5, 0xd5, 0xff),
            colors: [
                (0x2e, 0x34, 0x36), (0xcc, 0x00, 0x00), (0x3a, 0x7d, 0x00), (0x9a, 0x74, 0x00),
                (0x20, 0x4a, 0x87), (0x75, 0x50, 0x7b), (0x06, 0x78, 0x7a), (0xb8, 0xbd, 0xb5),
                (0x55, 0x57, 0x53), (0xef, 0x29, 0x29), (0x4e, 0x9a, 0x06), (0xc4, 0xa0, 0x00),
                (0x34, 0x65, 0xa4), (0xad, 0x7f, 0xa8), (0x06, 0x98, 0x9a), (0xee, 0xee, 0xec),
            ],
        }
    }

    pub fn dark() -> Palette {
        Palette {
            foreground: (0xd8, 0xdb, 0xd6),
            background: (0x16, 0x18, 0x1a),
            cursor: (0xd8, 0xdb, 0xd6),
            selection: (0x3a, 0x4d, 0x6b),
            colors: [
                (0x2e, 0x34, 0x36), (0xcc, 0x00, 0x00), (0x4e, 0x9a, 0x06), (0xc4, 0xa0, 0x00),
                (0x34, 0x65, 0xa4), (0x75, 0x50, 0x7b), (0x06, 0x98, 0x9a), (0xd3, 0xd7, 0xcf),
                (0x55, 0x57, 0x53), (0xef, 0x29, 0x29), (0x8a, 0xe2, 0x34), (0xfc, 0xe9, 0x4f),
                (0x72, 0x9f, 0xcf), (0xad, 0x7f, 0xa8), (0x34, 0xe2, 0xe2), (0xee, 0xee, 0xec),
            ],
        }
    }

    /// The 16 theme colours, extended with the standard xterm 256-colour tables.
    pub fn theme_colors(&self) -> ThemeColors {
        let mut palette = [(0, 0, 0); 256];
        palette[..16].copy_from_slice(&self.colors);
        let level = |i: usize| if i == 0 { 0 } else { (55 + i * 40) as u8 };
        for i in 0..216 {
            palette[16 + i] = (level(i / 36), level(i / 6 % 6), level(i % 6));
        }
        for i in 0..24 {
            let v = (8 + i * 10) as u8;
            palette[232 + i] = (v, v, v);
        }
        ThemeColors { fg: self.foreground, bg: self.background, cursor: self.cursor, palette }
    }

    pub fn set(&mut self, key: &str, c: Rgb) -> bool {
        match key {
            "foreground" => self.foreground = c,
            "background" => self.background = c,
            "cursor" => self.cursor = c,
            "selection" => self.selection = c,
            _ => match key.strip_prefix("color").and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if n < 16 => self.colors[n] = c,
                _ => return false,
            },
        }
        true
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    pub theme: ThemeChoice,
    pub scrollback: usize,
    pub scrollbar: bool,
    /// None means the desktop's choice; see `desktop::default_font`.
    pub font: Option<String>,
    pub term: String,
    pub light: Palette,
    pub dark: Palette,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theme: ThemeChoice::Follow,
            scrollback: 10000,
            scrollbar: true,
            font: None,
            term: "xterm-256color".to_string(),
            light: Palette::light(),
            dark: Palette::dark(),
        }
    }
}

/// Upper bound on the scrollback setting, to keep a typo from using all memory.
pub const MAX_SCROLLBACK: usize = 10_000_000;

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    /// Parse the file's text. Bad lines are reported and otherwise skipped, as
    /// one mistake should not keep a terminal from opening.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut errors = Vec::new();
        let mut section = String::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut bad = |what: &str| errors.push(format!("config line {}: {what}: {line}", i + 1));
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_string();
                if section != "light" && section != "dark" {
                    bad("unknown section");
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bad("expected key = value");
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let ok = match section.as_str() {
                "" => match key {
                    "theme" => ThemeChoice::parse(value).map(|t| cfg.theme = t).is_some(),
                    "scrollback" => match value.parse::<usize>() {
                        Ok(n) if n <= MAX_SCROLLBACK => {
                            cfg.scrollback = n;
                            true
                        }
                        _ => false,
                    },
                    "scrollbar" => parse_bool(value).map(|b| cfg.scrollbar = b).is_some(),
                    // Written by earlier versions, which could hide the title bar.
                    "titlebar" => true,
                    "font" => {
                        cfg.font = Some(value.to_string());
                        !value.is_empty()
                    }
                    "term" => {
                        cfg.term = value.to_string();
                        !value.is_empty()
                    }
                    _ => false,
                },
                "light" => parse_color(value).is_some_and(|c| cfg.light.set(key, c)),
                "dark" => parse_color(value).is_some_and(|c| cfg.dark.set(key, c)),
                _ => true,
            };
            if !ok {
                bad("not understood");
            }
        }
        (cfg, errors)
    }

    pub fn path() -> PathBuf {
        let base = env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("termgoo").join("config")
    }

    pub fn load() -> Config {
        let path = Config::path();
        let text = fs::read_to_string(&path).unwrap_or_default();
        let (cfg, errors) = Config::parse(&text);
        for e in errors {
            eprintln!("termgoo: {}: {e}", path.display());
        }
        cfg
    }

    /// Create the file with every setting listed, if it does not exist yet.
    pub fn ensure_file() -> std::io::Result<PathBuf> {
        let path = Config::path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, template())?;
        }
        Ok(path)
    }

    /// Change one setting in the file, keeping everything else in it. The section
    /// is "" for the top-level settings, or "light" or "dark". A value of None
    /// comments out the setting, so that its default applies.
    pub fn store(section: &str, key: &str, value: Option<&str>) -> std::io::Result<()> {
        let path = Config::ensure_file()?;
        let text = edit_setting(&fs::read_to_string(&path)?, section, key, value);
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }
}

fn section_name(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[').and_then(|l| l.strip_suffix(']')).map(str::trim)
}

fn edit_setting(text: &str, section: &str, key: &str, value: Option<&str>) -> String {
    let matches_key = |line: &str| {
        let l = line.trim().trim_start_matches('#').trim_start();
        l.split_once('=').is_some_and(|(k, _)| k.trim() == key)
    };
    let is_live = |line: &str| !line.trim().starts_with('#');
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    // The lines of the section, from just after its header to the next header.
    let start = if section.is_empty() {
        Some(0)
    } else {
        lines.iter().position(|l| section_name(l) == Some(section)).map(|i| i + 1)
    };
    let Some(start) = start else {
        if let Some(v) = value {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {v}"));
        }
        return lines.join("\n") + "\n";
    };
    let end = lines[start..].iter().position(|l| section_name(l).is_some()).map_or(lines.len(), |i| start + i);
    let range = &lines[start..end];
    let live = range.iter().position(|l| is_live(l) && matches_key(l)).map(|i| start + i);
    match value {
        Some(v) => {
            let new_line = format!("{key} = {v}");
            // Prefer replacing a live line, then the commented-out example.
            match live.or_else(|| range.iter().position(|l| matches_key(l)).map(|i| start + i)) {
                Some(i) => lines[i] = new_line,
                None => {
                    // After the section's last setting, before any blank lines.
                    let mut at = end;
                    while at > start && lines[at - 1].trim().is_empty() {
                        at -= 1;
                    }
                    lines.insert(if section.is_empty() { end } else { at }, new_line);
                }
            }
        }
        None => {
            if let Some(i) = live {
                lines[i] = format!("# {}", lines[i].trim());
            }
        }
    }
    lines.join("\n") + "\n"
}

pub fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn palette_text(name: &str, p: &Palette) -> String {
    let mut s = format!("[{name}]\n");
    for (k, c) in [
        ("foreground", p.foreground),
        ("background", p.background),
        ("cursor", p.cursor),
        ("selection", p.selection),
    ] {
        s += &format!("{k} = {}\n", hex(c));
    }
    for (i, c) in p.colors.iter().enumerate() {
        s += &format!("color{i} = {}\n", hex(*c));
    }
    s
}

fn template() -> String {
    let d = Config::default();
    format!(
        "# TermGoo configuration. \"Preferences\" in the right-click menu changes all of
# this. Changes made here by hand apply to windows opened afterwards. Colors are
# written #rrggbb.

# Terminal theme to start with: light, dark, or follow (the desktop's choice).
theme = {}

# Lines of scrollback to keep.
scrollback = {}

# Show the scrollbar.
scrollbar = {}

# Font, such as \"DejaVu Sans Mono 12\". Left unset, it comes from the desktop.
# font = Monospace 11

# What programs are told in TERM. The emulation stays the same whatever this says.
term = {}

{}
{}",
        d.theme.name(),
        d.scrollback,
        d.scrollbar,
        d.term,
        palette_text("light", &d.light),
        palette_text("dark", &d.dark),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_back_to_the_defaults() {
        let (cfg, errors) = Config::parse(&template());
        assert_eq!(errors, Vec::<String>::new());
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn settings_and_palettes() {
        let text = "theme = dark\nscrollback=5\n[light]\nbackground = #010203\ncolor15 = #ffffff\n[dark]\ncursor = #112233\n";
        let (cfg, errors) = Config::parse(text);
        assert!(errors.is_empty());
        assert_eq!(cfg.theme, ThemeChoice::Dark);
        assert_eq!(cfg.scrollback, 5);
        assert_eq!(cfg.light.background, (1, 2, 3));
        assert_eq!(cfg.light.colors[15], (255, 255, 255));
        assert_eq!(cfg.dark.cursor, (0x11, 0x22, 0x33));
        assert_eq!(cfg.dark.background, Palette::dark().background);
    }

    #[test]
    fn bad_lines_are_reported_and_skipped() {
        let (cfg, errors) = Config::parse("theme = pink\nnonsense\n[dark]\ncolor16 = #000000\nscrollback = 1\n");
        assert_eq!(errors.len(), 4);
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn retired_titlebar_setting_is_ignored_quietly() {
        let (cfg, errors) = Config::parse("titlebar = false\n");
        assert!(errors.is_empty());
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn edit_setting_keeps_the_rest() {
        let t = "# c\ntheme = dark\n\n[dark]\ntheme = x\n";
        assert_eq!(edit_setting(t, "", "theme", Some("light")), "# c\ntheme = light\n\n[dark]\ntheme = x\n");
        assert_eq!(edit_setting("# scrollbar = no\n", "", "scrollbar", Some("yes")), "scrollbar = yes\n");
        assert_eq!(edit_setting("a = 1\n[dark]\n", "", "b", Some("2")), "a = 1\nb = 2\n[dark]\n");
    }

    #[test]
    fn edit_setting_in_a_section() {
        let t = "theme = dark\n[light]\ncursor = #000000\n\n[dark]\ncursor = #111111\n";
        assert_eq!(
            edit_setting(t, "dark", "cursor", Some("#222222")),
            "theme = dark\n[light]\ncursor = #000000\n\n[dark]\ncursor = #222222\n"
        );
        assert_eq!(
            edit_setting(t, "light", "color3", Some("#333333")),
            "theme = dark\n[light]\ncursor = #000000\ncolor3 = #333333\n\n[dark]\ncursor = #111111\n"
        );
        assert_eq!(edit_setting("theme = dark\n", "dark", "cursor", Some("#010101")), "theme = dark\n\n[dark]\ncursor = #010101\n");
    }

    #[test]
    fn edit_setting_can_unset() {
        let t = "font = Mono 9\n# font = Monospace 11\n";
        assert_eq!(edit_setting(t, "", "font", None), "# font = Mono 9\n# font = Monospace 11\n");
        assert_eq!(edit_setting("[dark]\n", "", "font", None), "[dark]\n");
        let (cfg, _) = Config::parse(&edit_setting(t, "", "font", None));
        assert_eq!(cfg.font, None);
    }

    #[test]
    fn every_setting_survives_a_store() {
        let mut text = template();
        text = edit_setting(&text, "", "scrollback", Some("42"));
        text = edit_setting(&text, "", "font", Some("Mono 9"));
        text = edit_setting(&text, "dark", "color7", Some("#123456"));
        text = edit_setting(&text, "light", "selection", Some("#654321"));
        let (cfg, errors) = Config::parse(&text);
        assert!(errors.is_empty());
        assert_eq!(cfg.scrollback, 42);
        assert_eq!(cfg.font.as_deref(), Some("Mono 9"));
        assert_eq!(cfg.dark.colors[7], (0x12, 0x34, 0x56));
        assert_eq!(cfg.light.selection, (0x65, 0x43, 0x21));
        assert_eq!(cfg.light.colors[7], Palette::light().colors[7]);
    }

    #[test]
    fn cube_and_gray_ramp() {
        let c = Palette::dark().theme_colors();
        assert_eq!(c.palette[16], (0, 0, 0));
        assert_eq!(c.palette[196], (255, 0, 0));
        assert_eq!(c.palette[231], (255, 255, 255));
        assert_eq!(c.palette[232], (8, 8, 8));
        assert_eq!(c.palette[255], (238, 238, 238));
    }
}
