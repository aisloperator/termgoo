//! Command line handling. Options follow xterm: one dash, long names, and `-e`
//! taking everything after it as the program and its arguments.

use crate::config::{Config, ThemeChoice};

pub const USAGE: &str = "\
Usage: termgoo [options] [-e program [args...]]

  -title TEXT, -T TEXT  initial window title
  -e PROGRAM [ARGS...]  run PROGRAM instead of the shell; must be the last option
  -tn NAME, -term NAME  value of TERM for the program (default xterm-256color);
                        this changes only what is announced, never the emulation
  -theme THEME          terminal theme for this process: light, dark or follow
  -sl N, -scrollback N  lines of scrollback to keep
  -fn FONT, -font FONT  font, such as \"Monospace 11\"
  -geometry CxR         initial size in characters, such as 100x30
  -sb, +sb              show or hide the scrollbar
  -hold                 keep the window open after the program exits
  -help                 show this help
  -version              show the version
";

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Options {
    pub title: Option<String>,
    pub command: Option<Vec<String>>,
    pub term: Option<String>,
    pub theme: Option<ThemeChoice>,
    pub scrollback: Option<usize>,
    pub font: Option<String>,
    pub geometry: Option<(usize, usize)>,
    pub scrollbar: Option<bool>,
    pub hold: bool,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Options),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    let mut o = Options::default();
    while let Some(arg) = args.next() {
        // `--title` is accepted too, for people used to that convention.
        let name = arg.strip_prefix("--").map_or(arg.as_str(), |rest| &arg[arg.len() - rest.len() - 1..]);
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match name {
            "-title" | "-T" => o.title = Some(value()?),
            "-tn" | "-term" => o.term = Some(value()?),
            "-theme" => {
                let v = value()?;
                o.theme = Some(ThemeChoice::parse(&v).ok_or(format!("unknown theme: {v}"))?);
            }
            "-sl" | "-scrollback" => {
                let v = value()?;
                o.scrollback = Some(v.parse().map_err(|_| format!("bad number of lines: {v}"))?);
            }
            "-fn" | "-font" => o.font = Some(value()?),
            "-geometry" | "-g" => {
                let v = value()?;
                o.geometry = Some(parse_geometry(&v).ok_or(format!("bad geometry: {v}"))?);
            }
            "-sb" => o.scrollbar = Some(true),
            "+sb" => o.scrollbar = Some(false),
            "-hold" => o.hold = true,
            "-e" => {
                let command: Vec<String> = args.by_ref().collect();
                if command.is_empty() {
                    return Err("-e needs a program".into());
                }
                o.command = Some(command);
            }
            "-help" | "-h" => return Ok(Parsed::Help),
            "-version" | "-v" => return Ok(Parsed::Version),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    Ok(Parsed::Run(o))
}

fn parse_geometry(s: &str) -> Option<(usize, usize)> {
    let (c, r) = s.split_once(['x', 'X'])?;
    let (c, r) = (c.parse().ok()?, r.parse().ok()?);
    (c > 0 && r > 0 && c <= 1000 && r <= 1000).then_some((c, r))
}

impl Options {
    /// Options win over the configuration file, for this process only.
    pub fn apply(&self, cfg: &mut Config) {
        if let Some(t) = &self.term {
            cfg.term = t.clone();
        }
        if let Some(t) = self.theme {
            cfg.theme = t;
        }
        if let Some(n) = self.scrollback {
            cfg.scrollback = n;
        }
        if let Some(f) = &self.font {
            cfg.font = Some(f.clone());
        }
        if let Some(s) = self.scrollbar {
            cfg.scrollbar = s;
        }
    }

    /// As in xterm: an explicit title, else the program's name, else our own.
    pub fn window_title(&self) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        match self.command.as_ref().and_then(|c| c.first()) {
            Some(p) => p.rsplit('/').next().unwrap_or(p).to_string(),
            None => "termgoo".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Options {
        match parse(args.iter().map(|s| s.to_string())).unwrap() {
            Parsed::Run(o) => o,
            other => panic!("{other:?}"),
        }
    }

    fn err(args: &[&str]) -> String {
        parse(args.iter().map(|s| s.to_string())).unwrap_err()
    }

    #[test]
    fn dash_e_takes_the_rest_verbatim() {
        let o = run(&["-title", "x y", "-e", "vi", "-title", "-e", "--", "f"]);
        assert_eq!(o.title.as_deref(), Some("x y"));
        assert_eq!(o.command.unwrap(), ["vi", "-title", "-e", "--", "f"]);
    }

    #[test]
    fn title_follows_xterm() {
        assert_eq!(run(&[]).window_title(), "termgoo");
        assert_eq!(run(&["-e", "/usr/bin/top", "-d"]).window_title(), "top");
        assert_eq!(run(&["-T", "mine", "-e", "top"]).window_title(), "mine");
    }

    #[test]
    fn double_dash_and_values() {
        let o = run(&["--tn", "vt102", "-theme", "light", "-sl", "5", "-geometry", "100x30", "+sb"]);
        assert_eq!(o.term.as_deref(), Some("vt102"));
        assert_eq!(o.theme, Some(ThemeChoice::Light));
        assert_eq!(o.scrollback, Some(5));
        assert_eq!(o.geometry, Some((100, 30)));
        assert_eq!(o.scrollbar, Some(false));
    }

    #[test]
    fn errors() {
        assert_eq!(err(&["-e"]), "-e needs a program");
        assert_eq!(err(&["-title"]), "-title needs a value");
        assert_eq!(err(&["-theme", "blue"]), "unknown theme: blue");
        assert_eq!(err(&["-geometry", "0x5"]), "bad geometry: 0x5");
        assert_eq!(err(&["-sl", "x"]), "bad number of lines: x");
        assert_eq!(err(&["-nope"]), "unknown option: -nope");
    }
}
