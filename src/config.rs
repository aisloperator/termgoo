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

    fn set(&mut self, key: &str, c: Rgb) -> bool {
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
    pub titlebar: bool,
    pub font: String,
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
            titlebar: false,
            font: "Monospace 11".to_string(),
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
                    "titlebar" => parse_bool(value).map(|b| cfg.titlebar = b).is_some(),
                    "font" => {
                        cfg.font = value.to_string();
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

    /// Change one top-level setting in the file, keeping everything else in it.
    pub fn store(key: &str, value: &str) -> std::io::Result<()> {
        let path = Config::ensure_file()?;
        let text = edit_setting(&fs::read_to_string(&path)?, key, value);
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }
}

fn edit_setting(text: &str, key: &str, value: &str) -> String {
    let matches_key = |line: &str| {
        let l = line.trim().trim_start_matches('#').trim_start();
        l.split_once('=').is_some_and(|(k, _)| k.trim() == key)
    };
    let new_line = format!("{key} = {value}");
    let lines: Vec<&str> = text.lines().collect();
    let section_start = lines.iter().position(|l| l.trim().starts_with('[')).unwrap_or(lines.len());
    let top = &lines[..section_start];
    // Prefer replacing a live line, then the commented-out example.
    let live = top.iter().position(|l| !l.trim().starts_with('#') && matches_key(l));
    let found = live.or_else(|| top.iter().position(|l| matches_key(l)));
    let mut out: Vec<&str> = lines.clone();
    match found {
        Some(i) => out[i] = &new_line,
        None => out.insert(section_start, &new_line),
    }
    out.join("\n") + "\n"
}

fn hex((r, g, b): Rgb) -> String {
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
        "# TermGoo configuration. After editing, use \"Reload Configuration\" in the
# right-click menu. Colors are written #rrggbb.

# Terminal theme to start with: light, dark, or follow (the desktop's choice).
theme = {}

# Lines of scrollback to keep.
scrollback = {}

# Show the scrollbar, and the window's title bar.
scrollbar = {}
titlebar = {}

font = {}

# What programs are told in TERM. The emulation stays the same whatever this says.
term = {}

{}
{}",
        d.theme.name(),
        d.scrollback,
        d.scrollbar,
        d.titlebar,
        d.font,
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
    fn edit_setting_keeps_the_rest() {
        let t = "# c\ntheme = dark\n\n[dark]\ntheme = x\n";
        assert_eq!(edit_setting(t, "theme", "light"), "# c\ntheme = light\n\n[dark]\ntheme = x\n");
        assert_eq!(edit_setting("# scrollbar = no\n", "scrollbar", "yes"), "scrollbar = yes\n");
        assert_eq!(edit_setting("a = 1\n[dark]\n", "b", "2"), "a = 1\nb = 2\n[dark]\n");
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
