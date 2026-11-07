//! Fills an 80x24 terminal with TermGoo's display features, ready for a screenshot.
//!
//!     cargo build --release --example showcase
//!     target/release/termgoo -hold -e target/release/examples/showcase

use std::fmt::Write as _;
use std::io::Write as _;

type Rgb = (f64, f64, f64);

const SIXEL_WIDTH: usize = 640;
const SIXEL_HEIGHT: usize = 144;
/// Palette levels per channel. Their product stays within the 256 colour registers.
const LEVELS: [usize; 3] = [6, 7, 6];
const BAYER: [[f64; 4]; 4] =
    [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];

fn hue(h: f64) -> Rgb {
    let channel = |shift: f64| (((h + shift).rem_euclid(1.0) * 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0);
    (channel(0.0), channel(2.0 / 3.0), channel(1.0 / 3.0))
}

fn bytes((r, g, b): Rgb) -> (u8, u8, u8) {
    let v = |c: f64| (c.clamp(0.0, 1.0) * 255.0) as u8;
    (v(r), v(g), v(b))
}

fn fg(out: &mut String, c: Rgb) {
    let (r, g, b) = bytes(c);
    let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
}

fn bg(out: &mut String, c: Rgb) {
    let (r, g, b) = bytes(c);
    let _ = write!(out, "\x1b[48;2;{r};{g};{b}m");
}

/// A shaded sphere with a highlight, floating over a rainbow.
fn picture(x: usize, y: usize) -> Rgb {
    let (w, h) = (SIXEL_WIDTH as f64, SIXEL_HEIGHT as f64);
    let (fx, fy) = (x as f64 / w, y as f64 / h);
    let (dx, dy) = ((x as f64 - w / 2.0) / 62.0, (y as f64 - h / 2.0) / 62.0);
    let r2 = dx * dx + dy * dy;
    if r2 < 1.0 {
        let z = (1.0 - r2).sqrt();
        let light = (dx * -0.45 + dy * -0.55 + z * 0.7).max(0.0);
        let shine = light.powi(24);
        let shade = |c: f64| c * (0.15 + 0.85 * light) + shine;
        return (shade(0.2), shade(0.55), shade(0.95));
    }
    let (r, g, b) = hue(fx + fy * 0.15);
    let fade = 0.35 + 0.65 * fy;
    (r * fade, g * fade, b * fade)
}

/// The palette register for a colour, dithered so that gradients stay smooth.
fn register(c: Rgb, x: usize, y: usize) -> usize {
    let threshold = (BAYER[y % 4][x % 4] + 0.5) / 16.0;
    let level = |v: f64, n: usize| ((v.clamp(0.0, 1.0) * (n - 1) as f64 + threshold) as usize).min(n - 1);
    let (r, g, b) = (level(c.0, LEVELS[0]), level(c.1, LEVELS[1]), level(c.2, LEVELS[2]));
    (r * LEVELS[1] + g) * LEVELS[2] + b
}

fn sixel(out: &mut String) {
    let _ = write!(out, "\x1bPq\"1;1;{SIXEL_WIDTH};{SIXEL_HEIGHT}");
    for i in 0..LEVELS.iter().product() {
        let (b, rest) = (i % LEVELS[2], i / LEVELS[2]);
        let (g, r) = (rest % LEVELS[1], rest / LEVELS[1]);
        let percent = |v: usize, n: usize| v * 100 / (n - 1);
        let (r, g, b) = (percent(r, LEVELS[0]), percent(g, LEVELS[1]), percent(b, LEVELS[2]));
        let _ = write!(out, "#{i};2;{r};{g};{b}");
    }
    for top in (0..SIXEL_HEIGHT).step_by(6) {
        let cells: Vec<[usize; 6]> = (0..SIXEL_WIDTH)
            .map(|x| std::array::from_fn(|bit| register(picture(x, top + bit), x, top + bit)))
            .collect();
        let mut colors: Vec<usize> = cells.iter().flatten().copied().collect();
        colors.sort_unstable();
        colors.dedup();
        for color in colors {
            let _ = write!(out, "#{color}");
            let sixels: Vec<u8> = cells
                .iter()
                .map(|c| (0..6).filter(|&b| c[b] == color).fold(0, |bits, b| bits | 1 << b))
                .collect();
            // Run-length encoding keeps this to a size the terminal reads at once.
            for run in sixels.chunk_by(|a, b| a == b) {
                let ch = (run[0] + b'?') as char;
                if run.len() > 3 {
                    let _ = write!(out, "!{}{ch}", run.len());
                } else {
                    (0..run.len()).for_each(|_| out.push(ch));
                }
            }
            out.push('$');
        }
        out.push('-');
    }
    out.push_str("\x1b\\");
}

fn label(out: &mut String, name: &str) {
    let _ = write!(out, "\x1b[1m{name:<8}\x1b[0m");
}

fn title(out: &mut String) {
    let text = "TermGoo display showcase";
    out.push_str("\x1b[1m");
    let n = text.chars().count() as f64;
    for (i, c) in text.chars().enumerate() {
        fg(out, hue(i as f64 / n));
        out.push(c);
    }
    out.push_str("\x1b[0m\r\n\r\n");
}

fn styles(out: &mut String) {
    label(out, "Styles");
    out.push_str(concat!(
        "\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m ",
        "\x1b[4:2mdouble\x1b[0m \x1b[4:3;58;5;196mcurly\x1b[0m \x1b[4:4mdots\x1b[0m ",
        "\x1b[9mstrike\x1b[0m \x1b[53mover\x1b[0m \x1b[7minverse\x1b[0m\r\n",
    ));
}

fn palette(out: &mut String) {
    label(out, "16");
    for i in 0..8 {
        let _ = write!(out, "\x1b[{}m    ", 40 + i);
    }
    out.push_str("\x1b[0m    ");
    for i in 0..4 {
        let _ = write!(out, "\x1b[{}m Ab \x1b[0m", 91 + i);
    }
    out.push_str("\r\n");
    label(out, "");
    for i in 0..8 {
        let _ = write!(out, "\x1b[{}m    ", 100 + i);
    }
    out.push_str("\x1b[0m    ");
    for i in 0..4 {
        let _ = write!(out, "\x1b[{}m Ab \x1b[0m", 95 + i);
    }
    out.push_str("\r\n");
}

/// The 216-colour cube as six faces, in half blocks, then the gray ramp.
fn cube(out: &mut String) {
    for row in 0..3 {
        label(out, if row == 0 { "256" } else { "" });
        for face in 0..6 {
            for col in 0..6 {
                let index = |r: usize| 16 + face * 36 + r * 6 + col;
                let _ = write!(out, "\x1b[38;5;{};48;5;{}m▀", index(row * 2), index(row * 2 + 1));
            }
        }
        out.push_str("\x1b[0m  ");
        for i in 0..24 {
            let _ = write!(out, "\x1b[48;5;{}m ", 232 + i);
        }
        out.push_str("\x1b[0m\r\n");
    }
}

fn truecolor(out: &mut String) {
    const WIDTH: usize = 72;
    for row in 0..2 {
        label(out, if row == 0 { "24-bit" } else { "" });
        for x in 0..WIDTH {
            let along = x as f64 / WIDTH as f64;
            let shade = |level: f64| {
                let (r, g, b) = hue(along);
                (r * level, g * level, b * level)
            };
            fg(out, shade(1.0 - row as f64 * 0.5));
            bg(out, shade(0.75 - row as f64 * 0.5));
            out.push('▀');
        }
        out.push_str("\x1b[0m\r\n");
    }
}

fn unicode(out: &mut String) {
    label(out, "Unicode");
    out.push_str("café Ελληνικά Кириллица  漢字かなカナ  😀🚀🌈  e\u{301}n\u{303}o\u{308}  ∑∫√∞≠\r\n");
    label(out, "Boxes");
    out.push_str("┌──┬──┐ ╔══╦══╗ ╭────╮ ░▒▓█▇▆▅▄▃▂▁  ⠋⠙⠹⠸⠼⠴  \x1b(0lqqwqqk\x1b(B\r\n");
    label(out, "");
    out.push_str("└──┴──┘ ╚══╩══╝ ╰────╯ ←↑→↓ ⇐⇑⇒⇓ ♠♥♦♣ ♪♫ ✓✗  \x1b(0mqqvqqj\x1b(B\r\n");
}

fn main() -> std::io::Result<()> {
    let mut out = String::from("\x1b[2J\x1b[H");
    title(&mut out);
    styles(&mut out);
    palette(&mut out);
    cube(&mut out);
    truecolor(&mut out);
    unicode(&mut out);
    label(&mut out, "Sixel");
    sixel(&mut out);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()
}
